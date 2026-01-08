use crate::{VmError, VmState, path_to_str};
use intar_core::VmDefinition;
use std::fs::File;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::AsyncBufRead;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MAIN_DISK_NODE_NAME: &str = "intar_disk0";
const CLOUD_INIT_NODE_NAME: &str = "intar_cloud_init0";
const SNAPSHOT_JOB_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SNAPSHOT_JOB_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub enum SharedNetworkEndpoint {
    /// Per-scenario UDP datagram L2 segment routed through an intar-managed switch.
    ///
    /// Uses QEMU's `-netdev dgram` backend with localhost UDP sockets, so it is
    /// privilege-free and works on macOS, Linux, and Windows.
    Dgram { hub_port: u16, local_port: u16 },
}

pub struct QemuInstance {
    pub name: String,
    pub definition: VmDefinition,
    pub state: VmState,
    pub ssh_port: u16,
    pub mgmt_ip: String,
    pub shared_lan: Option<SharedNetworkEndpoint>,
    pub primary_mac: Option<String>,
    pub lan_mac: Option<String>,
    pub qmp_socket: PathBuf,
    pub serial_socket: PathBuf,
    pub actions_socket: PathBuf,
    pub pid_file: PathBuf,
    pub disk_path: PathBuf,
    pub base_image: Option<PathBuf>,
    pub cloud_init_iso: PathBuf,
    pub logs_dir: PathBuf,
    process: Option<Child>,
}

impl QemuInstance {
    #[must_use]
    pub fn new(
        definition: VmDefinition,
        work_dir: &Path,
        ssh_port: u16,
        mgmt_ip: String,
        shared_lan: Option<SharedNetworkEndpoint>,
        primary_mac: Option<String>,
        lan_mac: Option<String>,
    ) -> Self {
        let name = definition.name.clone();
        let logs_dir = work_dir.join("logs").join(&name);
        Self {
            name: name.clone(),
            definition,
            state: VmState::Starting,
            ssh_port,
            mgmt_ip,
            shared_lan,
            primary_mac,
            lan_mac,
            qmp_socket: work_dir.join(format!("{name}-qmp.sock")),
            serial_socket: work_dir.join(format!("{name}-serial.sock")),
            actions_socket: work_dir.join(format!("{name}-actions.sock")),
            pid_file: work_dir.join(format!("{name}-qemu.pid")),
            disk_path: work_dir.join(format!("{name}.qcow2")),
            base_image: None,
            cloud_init_iso: work_dir.join(format!("{name}-cloud-init.iso")),
            logs_dir,
            process: None,
        }
    }

    /// Create a qcow2 overlay referencing the base image.
    ///
    /// # Errors
    /// Returns `VmError` if `qemu-img` fails or paths cannot be prepared.
    pub fn create_overlay_disk(&mut self, base_image: &Path) -> Result<(), VmError> {
        self.base_image = Some(base_image.to_path_buf());

        let output = Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                "-b",
                path_to_str(base_image)?,
                "-F",
                "qcow2",
                path_to_str(&self.disk_path)?,
                &format!("{}G", self.definition.disk),
            ])
            .output()
            .map_err(|e| VmError::Qemu(format!("Failed to create disk: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VmError::Qemu(format!("qemu-img create failed: {stderr}")));
        }

        Ok(())
    }

    /// Recreate the overlay disk from the base image.
    ///
    /// # Errors
    /// Returns `VmError` if the overlay cannot be created.
    pub fn recreate_overlay_disk(&self) -> Result<(), VmError> {
        let base_image = self
            .base_image
            .as_ref()
            .ok_or_else(|| VmError::Qemu("No base image set".into()))?;

        if self.disk_path.exists() {
            std::fs::remove_file(&self.disk_path)?;
        }

        let output = Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                "-b",
                path_to_str(base_image)?,
                "-F",
                "qcow2",
                path_to_str(&self.disk_path)?,
                &format!("{}G", self.definition.disk),
            ])
            .output()
            .map_err(|e| VmError::Qemu(format!("Failed to recreate disk: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VmError::Qemu(format!("qemu-img create failed: {stderr}")));
        }

        Ok(())
    }

    /// Start the QEMU process for this VM.
    ///
    /// # Errors
    /// Returns `VmError` if QEMU fails to launch.
    pub fn start(&mut self, arch: &str) -> Result<(), VmError> {
        std::fs::create_dir_all(&self.logs_dir)?;

        let qemu_binary = Self::qemu_binary_for_arch(arch)?;
        let mut cmd = Command::new(qemu_binary);
        self.configure_qemu_command(&mut cmd, arch);
        self.redirect_qemu_output(&mut cmd)?;

        let child = cmd
            .spawn()
            .map_err(|e| VmError::Qemu(format!("Failed to start QEMU: {e}")))?;

        if let Err(e) = std::fs::write(&self.pid_file, child.id().to_string()) {
            return Err(VmError::Qemu(format!(
                "Failed to write QEMU PID file {}: {e}",
                self.pid_file.display()
            )));
        }

        self.process = Some(child);
        self.state = VmState::Booting;

        Ok(())
    }

    fn qemu_binary_for_arch(arch: &str) -> Result<&'static str, VmError> {
        match arch {
            "x86_64" | "amd64" => Ok("qemu-system-x86_64"),
            "aarch64" | "arm64" => Ok("qemu-system-aarch64"),
            _ => Err(VmError::Qemu(format!("Unsupported architecture: {arch}"))),
        }
    }

    fn configure_qemu_command(&self, cmd: &mut Command, arch: &str) {
        cmd.args(["-name", &self.name]);

        Self::apply_machine_args(cmd, arch);
        self.apply_resource_args(cmd);
        self.apply_drive_args(cmd);
        Self::apply_rng_args(cmd);
        self.apply_network_args(cmd);
        self.apply_agent_serial_args(cmd);
        self.apply_console_args(cmd);
        self.apply_qmp_args(cmd);
        Self::apply_misc_args(cmd);
    }

    fn apply_machine_args(cmd: &mut Command, arch: &str) {
        match arch {
            "aarch64" | "arm64" => {
                cmd.args(["-machine", "virt,highmem=on"]);
                cmd.args(["-cpu", "host"]);

                let efi_paths = [
                    "/opt/homebrew/share/qemu/edk2-aarch64-code.fd",
                    "/usr/share/qemu/edk2-aarch64-code.fd",
                    "/usr/share/AAVMF/AAVMF_CODE.fd",
                ];
                if let Some(efi_path) = efi_paths.iter().find(|p| Path::new(p).exists()) {
                    cmd.args(["-bios", efi_path]);
                }
            }
            "x86_64" | "amd64" => {
                cmd.args(["-machine", "q35"]);
                cmd.args(["-cpu", "host"]);
            }
            _ => {}
        }
    }

    fn apply_resource_args(&self, cmd: &mut Command) {
        cmd.args(["-m", &format!("{}M", self.definition.memory)]);
        cmd.args(["-smp", &self.definition.cpu.to_string()]);
    }

    fn apply_drive_args(&self, cmd: &mut Command) {
        cmd.args([
            "-drive",
            &format!(
                "file={},format=qcow2,if=virtio,node-name={MAIN_DISK_NODE_NAME}",
                self.disk_path.display()
            ),
        ]);

        cmd.args([
            "-drive",
            &format!(
                "file={},format=raw,if=virtio,readonly=on,node-name={CLOUD_INIT_NODE_NAME}",
                self.cloud_init_iso.display()
            ),
        ]);
    }

    fn apply_rng_args(cmd: &mut Command) {
        // Provide a virtio RNG to avoid entropy-related boot stalls.
        if !cfg!(target_os = "windows") {
            cmd.args(["-object", "rng-random,id=rng0,filename=/dev/urandom"]);
            cmd.args(["-device", "virtio-rng-pci,rng=rng0"]);
        }
    }

    fn apply_network_args(&self, cmd: &mut Command) {
        cmd.args([
            "-netdev",
            &format!(
                "user,id=net0,hostfwd=tcp::{}-{}:22",
                self.ssh_port, self.mgmt_ip
            ),
        ]);

        let mut net0 = String::from("virtio-net-pci,netdev=net0");
        if let Some(mac) = &self.primary_mac {
            net0.push_str(",mac=");
            net0.push_str(mac);
        }
        cmd.args(["-device", &net0]);

        if let Some(lan) = &self.shared_lan {
            let netdev = match lan {
                SharedNetworkEndpoint::Dgram {
                    hub_port,
                    local_port,
                } => format!(
                    "dgram,id=net1,local.type=inet,local.host=127.0.0.1,local.port={local_port},remote.type=inet,remote.host=127.0.0.1,remote.port={hub_port}"
                ),
            };
            cmd.args(["-netdev", &netdev]);

            let mut net1 = String::from("virtio-net-pci,netdev=net1");
            if let Some(mac) = &self.lan_mac {
                net1.push_str(",mac=");
                net1.push_str(mac);
            }
            cmd.args(["-device", &net1]);
        }
    }

    fn apply_agent_serial_args(&self, cmd: &mut Command) {
        cmd.args(["-device", "virtio-serial-pci,id=virtio-serial0"]);
        cmd.args([
            "-chardev",
            &format!(
                "socket,id=agent,path={},server=on,wait=off",
                self.serial_socket.display()
            ),
        ]);
        cmd.args(["-device", "virtserialport,chardev=agent,name=intar.agent"]);

        cmd.args([
            "-chardev",
            &format!(
                "socket,id=actions,path={},server=on,wait=off",
                self.actions_socket.display()
            ),
        ]);
        cmd.args([
            "-device",
            "virtserialport,chardev=actions,name=intar.actions",
        ]);
    }

    fn apply_console_args(&self, cmd: &mut Command) {
        let console_log_path = self.logs_dir.join("console.log");
        cmd.args([
            "-chardev",
            &format!("file,id=console,path={}", console_log_path.display()),
        ]);
        cmd.args(["-serial", "chardev:console"]);
    }

    fn apply_qmp_args(&self, cmd: &mut Command) {
        cmd.args([
            "-qmp",
            &format!("unix:{},server,nowait", self.qmp_socket.display()),
        ]);
    }

    fn apply_misc_args(cmd: &mut Command) {
        cmd.args(["-display", "none"]);

        if cfg!(target_os = "macos") {
            cmd.args(["-accel", "hvf"]);
        } else if cfg!(target_os = "linux") {
            cmd.args(["-enable-kvm"]);
        }
    }

    fn redirect_qemu_output(&self, cmd: &mut Command) -> Result<(), VmError> {
        let qemu_log_path = self.logs_dir.join("qemu.log");
        let qemu_log = File::create(&qemu_log_path)
            .map_err(|e| VmError::Qemu(format!("Failed to create qemu.log: {e}")))?;
        let qemu_log_err = qemu_log
            .try_clone()
            .map_err(|e| VmError::Qemu(format!("Failed to clone log file handle: {e}")))?;

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::from(qemu_log));
        cmd.stderr(Stdio::from(qemu_log_err));

        Ok(())
    }

    /// Send a QMP command and return the JSON response.
    ///
    /// # Errors
    /// Returns `VmError::Qmp` on communication or parsing failures.
    pub async fn qmp_command(
        &self,
        command: &str,
        args: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, VmError> {
        let stream = UnixStream::connect(&self.qmp_socket)
            .await
            .map_err(|e| VmError::Qmp(format!("Failed to connect to QMP: {e}")))?;

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        let _greeting = Self::read_qmp_greeting(&mut reader).await?;

        let capabilities = r#"{"execute": "qmp_capabilities"}"#;
        write_half
            .write_all(format!("{capabilities}\n").as_bytes())
            .await
            .map_err(|e| VmError::Qmp(format!("Failed to send capabilities: {e}")))?;

        let cap_response = Self::read_qmp_response(&mut reader).await?;
        if let Some(err) = cap_response.get("error") {
            return Err(VmError::Qmp(format!("qmp_capabilities error: {err}")));
        }

        let cmd_json = if let Some(args) = args {
            serde_json::json!({
                "execute": command,
                "arguments": args
            })
        } else {
            serde_json::json!({
                "execute": command
            })
        };

        write_half
            .write_all(format!("{cmd_json}\n").as_bytes())
            .await
            .map_err(|e| VmError::Qmp(format!("Failed to send command: {e}")))?;

        Self::read_qmp_response(&mut reader).await
    }

    async fn read_qmp_message<R: AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> Result<serde_json::Value, VmError> {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .await
            .map_err(|e| VmError::Qmp(format!("Failed to read QMP message: {e}")))?;
        if bytes == 0 {
            return Err(VmError::Qmp("Unexpected EOF reading QMP message".into()));
        }

        serde_json::from_str(&line).map_err(|e| VmError::Qmp(format!("Invalid QMP JSON: {e}")))
    }

    async fn read_qmp_greeting<R: AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> Result<serde_json::Value, VmError> {
        loop {
            let message = Self::read_qmp_message(reader).await?;
            if message.get("QMP").is_some() {
                return Ok(message);
            }
            if message.get("event").is_some() {
                continue;
            }
            return Err(VmError::Qmp(format!("Unexpected QMP greeting: {message}")));
        }
    }

    async fn read_qmp_response<R: AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> Result<serde_json::Value, VmError> {
        loop {
            let message = Self::read_qmp_message(reader).await?;
            if message.get("event").is_some() {
                continue;
            }
            if message.get("return").is_some() || message.get("error").is_some() {
                return Ok(message);
            }
        }
    }

    /// Save a QEMU checkpoint named `name`.
    ///
    /// # Errors
    /// Returns `VmError::Qmp` if the command fails.
    pub async fn save_checkpoint(&self, name: &str) -> Result<(), VmError> {
        let job_id = format!("intar_snapshot_save_{}_{}", self.name, name);
        let response: serde_json::Value = self
            .qmp_command(
                "snapshot-save",
                Some(serde_json::json!({
                    "job-id": job_id.clone(),
                    "tag": name,
                    "vmstate": MAIN_DISK_NODE_NAME,
                    "devices": [MAIN_DISK_NODE_NAME],
                })),
            )
            .await?;

        if let Some(err) = response.get("error") {
            return Err(VmError::Qmp(format!("snapshot-save error: {err}")));
        }

        self.wait_for_job(&job_id).await
    }

    /// Load a previously saved QEMU checkpoint.
    ///
    /// # Errors
    /// Returns `VmError::Qmp` if the command fails.
    pub async fn load_checkpoint(&self, name: &str) -> Result<(), VmError> {
        let job_id = format!("intar_snapshot_load_{}_{}", self.name, name);
        let response: serde_json::Value = self
            .qmp_command(
                "snapshot-load",
                Some(serde_json::json!({
                    "job-id": job_id.clone(),
                    "tag": name,
                    "vmstate": MAIN_DISK_NODE_NAME,
                    "devices": [MAIN_DISK_NODE_NAME],
                })),
            )
            .await?;

        if let Some(err) = response.get("error") {
            return Err(VmError::Qmp(format!("snapshot-load error: {err}")));
        }

        self.wait_for_job(&job_id).await
    }

    async fn wait_for_job(&self, job_id: &str) -> Result<(), VmError> {
        let deadline = Instant::now() + SNAPSHOT_JOB_TIMEOUT;

        loop {
            let response: serde_json::Value = self.qmp_command("query-jobs", None).await?;
            if let Some(err) = response.get("error") {
                return Err(VmError::Qmp(format!("query-jobs error: {err}")));
            }

            let jobs = response
                .get("return")
                .and_then(|v| v.as_array())
                .ok_or_else(|| VmError::Qmp("query-jobs returned unexpected payload".into()))?;

            if let Some(job) = jobs.iter().find(|job| {
                job.get("id")
                    .and_then(|id| id.as_str())
                    .is_some_and(|id| id == job_id)
            }) {
                let status = job.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "concluded" {
                    let dismiss_response: serde_json::Value = self
                        .qmp_command(
                            "job-dismiss",
                            Some(serde_json::json!({
                                "id": job_id
                            })),
                        )
                        .await?;
                    if let Some(err) = dismiss_response.get("error") {
                        return Err(VmError::Qmp(format!("job-dismiss error: {err}")));
                    }

                    if let Some(error) = job.get("error")
                        && !error.is_null()
                    {
                        let message =
                            error
                                .as_str()
                                .map(ToString::to_string)
                                .or_else(|| {
                                    let desc = error.get("desc")?.as_str()?;
                                    let class = error.get("class").and_then(|v| v.as_str());
                                    Some(class.map_or_else(
                                        || desc.to_string(),
                                        |c| format!("{c}: {desc}"),
                                    ))
                                })
                                .unwrap_or_else(|| error.to_string());

                        return Err(VmError::Qmp(format!("Job '{job_id}' failed: {message}")));
                    }

                    return Ok(());
                }
            }

            if Instant::now() >= deadline {
                return Err(VmError::Qmp(format!("Timed out waiting for job: {job_id}")));
            }

            tokio::time::sleep(SNAPSHOT_JOB_POLL_INTERVAL).await;
        }
    }

    /// Reset the VM (reboot guest)
    ///
    /// # Errors
    /// Returns `VmError::Qmp` if the QMP command fails.
    pub async fn system_reset(&self) -> Result<(), VmError> {
        let response: serde_json::Value = self.qmp_command("system_reset", None).await?;

        if let Some(err) = response.get("error") {
            return Err(VmError::Qmp(format!("system_reset failed: {err}")));
        }

        Ok(())
    }

    /// Pause guest CPUs.
    ///
    /// # Errors
    /// Returns `VmError::Qmp` if the QMP command fails.
    pub async fn pause(&self) -> Result<(), VmError> {
        let response: serde_json::Value = self.qmp_command("stop", None).await?;

        if let Some(err) = response.get("error") {
            return Err(VmError::Qmp(format!("stop failed: {err}")));
        }

        Ok(())
    }

    /// Resume guest CPUs.
    ///
    /// # Errors
    /// Returns `VmError::Qmp` if the QMP command fails.
    pub async fn resume(&self) -> Result<(), VmError> {
        let response: serde_json::Value = self.qmp_command("cont", None).await?;

        if let Some(err) = response.get("error") {
            return Err(VmError::Qmp(format!("cont failed: {err}")));
        }

        Ok(())
    }

    /// Stop the QEMU process.
    ///
    /// # Errors
    /// Returns `VmError` if QMP `quit` fails; ignores errors while killing the child.
    pub async fn stop(&mut self) -> Result<(), VmError> {
        self.qmp_command("quit", None).await.ok();

        if let Some(mut child) = self.process.take() {
            let deadline = Instant::now() + Duration::from_secs(5);

            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            child.kill().ok();
                            child.wait().ok();
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(_) => {
                        child.kill().ok();
                        child.wait().ok();
                        break;
                    }
                }
            }
        }

        for path in [
            &self.qmp_socket,
            &self.serial_socket,
            &self.actions_socket,
            &self.pid_file,
        ] {
            if path.exists() {
                std::fs::remove_file(path).ok();
            }
        }

        Ok(())
    }
}

impl Drop for QemuInstance {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            child.kill().ok();
        }

        if self.pid_file.exists() {
            std::fs::remove_file(&self.pid_file).ok();
        }
    }
}

/// Find an available localhost TCP port.
///
/// # Errors
/// Returns `VmError::NoFreePort` if binding a temporary listener fails.
pub fn find_free_port() -> Result<u16, VmError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| VmError::NoFreePort)?;
    let port = listener
        .local_addr()
        .map_err(|_| VmError::NoFreePort)?
        .port();
    Ok(port)
}

/// Find an available localhost UDP port.
///
/// # Errors
/// Returns `VmError::NoFreePort` if binding a temporary UDP socket fails.
pub fn find_free_udp_port() -> Result<u16, VmError> {
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(|_| VmError::NoFreePort)?;
    let port = socket.local_addr().map_err(|_| VmError::NoFreePort)?.port();
    Ok(port)
}

/// Find `count` available localhost TCP ports.
///
/// # Errors
/// Propagates `VmError::NoFreePort` if a free port cannot be found.
pub fn find_free_ports(count: usize) -> Result<Vec<u16>, VmError> {
    let mut ports = Vec::with_capacity(count);
    for _ in 0..count {
        ports.push(find_free_port()?);
    }
    Ok(ports)
}
