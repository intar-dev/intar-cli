use crate::{
    ActionLineEvent, CloudInitGenerator, HostSocket, ImageCache, IntarDirs, LanSwitch,
    QemuInstance, QemuInstanceConfig, QemuSockets, ScenarioState, SharedNetworkEndpoint, VmError,
    VmState, find_free_ports, find_free_udp_port, path_to_str, start_vm_actions_task, try_connect,
};
use intar_core::{CloudInitConfig, ProbePhase, Scenario, VmDefinition, WriteFile};
use intar_probes::{ProbeResult, ProbeSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tracing::{error, info, warn};

use crate::apply_vm_steps_to_cloud_init;
use futures_util::future::try_join_all;

const NET_SETUP_SCRIPT_PREFIX: &str = r#"#!/usr/bin/env bash
set -euo pipefail

iface_for_mac() {
  local mac="$1"
  for p in /sys/class/net/*; do
    local name addr
    name="$(basename "$p")"
    addr="$(cat "$p/address" 2>/dev/null || true)"
    if [ "$addr" = "$mac" ]; then
      echo "$name"
      return 0
    fi
  done
  return 1
}

"#;

const NET_SETUP_SCRIPT_RENAME_AND_FALLBACKS: &str = r#"
# Fallbacks if names aren't ready yet.
[ -z "$MGMT_IF" ] && MGMT_IF="enp0s1"
[ -n "$LAN_MAC" ] && [ -z "$LAN_IF" ] && LAN_IF="enp0s2"

exists_if() { [ -d "/sys/class/net/$1" ]; }

# Ensure stable names for scenario scripts.
TMP_MGMT="intar-mgmt0"

if exists_if "$MGMT_IF" && [ "$MGMT_IF" != "enp0s1" ]; then
  ip link set "$MGMT_IF" down 2>/dev/null || true
  ip link set "$MGMT_IF" name "$TMP_MGMT" 2>/dev/null || true
  MGMT_IF="$TMP_MGMT"
fi

if [ -n "$LAN_IF" ] && exists_if "$LAN_IF" && [ "$LAN_IF" != "enp0s2" ]; then
  ip link set "$LAN_IF" down 2>/dev/null || true
  ip link set "$LAN_IF" name "enp0s2" 2>/dev/null || true
  LAN_IF="enp0s2"
fi

if exists_if "$MGMT_IF" && [ "$MGMT_IF" != "enp0s1" ]; then
  ip link set "$MGMT_IF" down 2>/dev/null || true
  ip link set "$MGMT_IF" name "enp0s1" 2>/dev/null || true
  MGMT_IF="enp0s1"
fi
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInfo {
    pub name: String,
    pub ssh_port: u16,
    pub image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub scenario_name: String,
    pub vms: Vec<VmInfo>,
}

impl RunState {
    /// Load run state from disk.
    ///
    /// # Errors
    /// Returns `VmError` if the state file cannot be read or parsed.
    pub fn load(run_dir: &Path) -> Result<Self, VmError> {
        let state_file = run_dir.join("state.json");
        let content = std::fs::read_to_string(&state_file)?;
        serde_json::from_str(&content)
            .map_err(|e| VmError::Qemu(format!("Failed to parse state: {e}")))
    }

    /// Save run state to disk.
    ///
    /// # Errors
    /// Returns `VmError` if the state cannot be serialized or written.
    pub fn save(&self, run_dir: &Path) -> Result<(), VmError> {
        let state_file = run_dir.join("state.json");
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| VmError::Qemu(format!("Failed to serialize state: {e}")))?;
        std::fs::write(&state_file, content)?;
        Ok(())
    }
}

pub struct ScenarioRunner {
    pub scenario: Scenario,
    pub state: ScenarioState,
    pub vms: HashMap<String, QemuInstance>,
    pub probe_results: HashMap<String, HashMap<String, ProbeResult>>,
    pub vm_order: Vec<String>,
    pub work_dir: PathBuf,
    pub ssh_private_key: String,
    pub ssh_public_key: String,
    pub vm_addresses: HashMap<String, String>,
    agent_binary_x86_64: Vec<u8>,
    agent_binary_aarch64: Vec<u8>,
    ports: Vec<u16>,
    port_index: usize,
    shared_lan_hub_port: Option<u16>,
    lan_switch: Option<LanSwitch>,
    action_rx: Option<mpsc::Receiver<ActionLineEvent>>,
    action_tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl ScenarioRunner {
    /// Build a runner using default directories.
    ///
    /// # Errors
    /// Returns `VmError` if directory initialization fails.
    pub fn new(
        scenario: Scenario,
        agent_binary_x86_64: Vec<u8>,
        agent_binary_aarch64: Vec<u8>,
    ) -> Result<Self, VmError> {
        let dirs = IntarDirs::new()?;
        Self::new_with_dirs(scenario, agent_binary_x86_64, agent_binary_aarch64, &dirs)
    }

    /// Build a runner using explicit directories (useful for tests).
    ///
    /// # Errors
    /// Returns `VmError` if directory setup fails.
    pub fn new_with_dirs(
        scenario: Scenario,
        agent_binary_x86_64: Vec<u8>,
        agent_binary_aarch64: Vec<u8>,
        dirs: &IntarDirs,
    ) -> Result<Self, VmError> {
        dirs.ensure_dirs()?;

        let work_dir = dirs.new_run_dir();
        std::fs::create_dir_all(&work_dir)?;

        let (private_key, public_key) = generate_ssh_keypair(&work_dir)?;

        let port_count = if cfg!(target_os = "windows") {
            scenario.vms.len() * 4
        } else {
            scenario.vms.len()
        };
        let ports = find_free_ports(port_count)?;
        let shared_lan_hub_port = if scenario.vms.len() > 1 {
            Some(find_free_udp_port()?)
        } else {
            None
        };

        let vm_addresses = Self::assign_vm_addresses(&scenario)?;

        Ok(Self {
            scenario,
            state: ScenarioState::Initializing,
            vms: HashMap::new(),
            probe_results: HashMap::new(),
            vm_order: Vec::new(),
            work_dir,
            ssh_private_key: private_key,
            ssh_public_key: public_key,
            vm_addresses,
            agent_binary_x86_64,
            agent_binary_aarch64,
            ports,
            port_index: 0,
            shared_lan_hub_port,
            lan_switch: None,
            action_rx: None,
            action_tasks: Vec::new(),
        })
    }

    /// Start streaming SSH action events from all VMs.
    ///
    /// # Errors
    /// Returns `VmError` if action recording cannot be started.
    pub fn start_action_recording(&mut self) -> Result<(), VmError> {
        if self.action_rx.is_some() {
            return Ok(());
        }

        let (tx, rx) = mpsc::channel::<ActionLineEvent>(1024);
        self.action_rx = Some(rx);

        for (name, vm) in &self.vms {
            let handle = start_vm_actions_task(
                name.clone(),
                vm.actions_socket.clone(),
                vm.logs_dir.join("ssh-actions.ndjson"),
                tx.clone(),
            );
            self.action_tasks.push(handle);
        }

        Ok(())
    }

    #[must_use]
    pub fn drain_action_lines(&mut self) -> Vec<ActionLineEvent> {
        let Some(rx) = self.action_rx.as_mut() else {
            return Vec::new();
        };

        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Prepare a VM: pick ports, create overlay disk, and generate cloud-init assets.
    ///
    /// # Errors
    /// Returns `VmError` if resources are missing or disk/cloud-init creation fails.
    pub fn create_vm(
        &mut self,
        vm_def: &VmDefinition,
        image_cache: &ImageCache,
        arch: &str,
    ) -> Result<(), VmError> {
        let ssh_port = self.next_port()?;
        let shared_ep = if let Some(hub_port) = self.shared_lan_hub_port {
            let local_port = find_free_udp_port()?;
            Some(SharedNetworkEndpoint::Dgram {
                hub_port,
                local_port,
            })
        } else {
            None
        };
        let has_shared_lan = shared_ep.is_some();

        let vm_index = self.vm_order.len();
        let (primary_mac, lan_mac) = Self::generate_macs(vm_index)?;
        let primary_mac_for_cfg = primary_mac.clone();
        let lan_mac_for_cfg = lan_mac.clone();
        let mgmt_ip = Self::mgmt_ip(vm_index)?;
        #[cfg(unix)]
        let qmp_socket = self.host_socket_for_vm(&vm_def.name, "qmp");
        #[cfg(windows)]
        let qmp_socket = self.host_socket_for_vm(&vm_def.name, "qmp")?;

        #[cfg(unix)]
        let serial_socket = self.host_socket_for_vm(&vm_def.name, "serial");
        #[cfg(windows)]
        let serial_socket = self.host_socket_for_vm(&vm_def.name, "serial")?;

        #[cfg(unix)]
        let actions_socket = self.host_socket_for_vm(&vm_def.name, "actions");
        #[cfg(windows)]
        let actions_socket = self.host_socket_for_vm(&vm_def.name, "actions")?;

        let mut vm = QemuInstance::new(
            QemuInstanceConfig {
                definition: vm_def.clone(),
                ssh_port,
                mgmt_ip: mgmt_ip.clone(),
                shared_lan: shared_ep,
                primary_mac: Some(primary_mac),
                lan_mac,
                sockets: QemuSockets {
                    qmp: qmp_socket,
                    serial: serial_socket,
                    actions: actions_socket,
                },
            },
            &self.work_dir,
        );
        let base_image = self.base_image_for_vm(vm_def, image_cache, arch)?;
        vm.create_overlay_disk(&base_image)?;
        let agent_binary = self.agent_binary_for_arch(arch)?;
        let cloud_init_gen =
            CloudInitGenerator::new(self.ssh_public_key.clone(), agent_binary.clone());
        let cloud_init_config = self.build_cloud_init_config(
            vm_def,
            &primary_mac_for_cfg,
            &mgmt_ip,
            lan_mac_for_cfg.as_deref(),
            has_shared_lan,
        )?;
        cloud_init_gen.save_to_logs(&cloud_init_config, &vm_def.name, &vm.logs_dir)?;
        cloud_init_gen.create_iso(&cloud_init_config, &vm_def.name, &vm.cloud_init_iso)?;

        self.vms.insert(vm_def.name.clone(), vm);
        self.probe_results
            .insert(vm_def.name.clone(), HashMap::new());
        self.vm_order.push(vm_def.name.clone());
        Ok(())
    }

    fn build_cloud_init_config(
        &self,
        vm_def: &VmDefinition,
        primary_mac: &str,
        mgmt_ip: &str,
        lan_mac: Option<&str>,
        has_shared_lan: bool,
    ) -> Result<CloudInitConfig, VmError> {
        let mut cloud_init_config = vm_def.cloud_init.clone().unwrap_or_default();
        apply_vm_steps_to_cloud_init(&vm_def.name, &vm_def.steps, &mut cloud_init_config)?;

        let hosts_content = self.render_hosts_file()?;
        cloud_init_config.write_files.push(WriteFile {
            path: "/etc/hosts.intar".into(),
            content: hosts_content,
            permissions: Some("0644".into()),
        });

        let cluster_ip = if has_shared_lan {
            Some(
                self.vm_addresses
                    .get(&vm_def.name)
                    .ok_or_else(|| VmError::Qemu("Missing address for VM".into()))?
                    .clone(),
            )
        } else {
            None
        };

        cloud_init_config.network_config = Some(Self::netplan_config(
            primary_mac,
            mgmt_ip,
            cluster_ip.as_deref().zip(lan_mac),
        )?);

        // Disable IPv6 system-wide.
        cloud_init_config.write_files.push(WriteFile {
            path: "/etc/sysctl.d/99-intar-no-ipv6.conf".into(),
            content:
                "net.ipv6.conf.all.disable_ipv6 = 1\nnet.ipv6.conf.default.disable_ipv6 = 1\nnet.ipv6.conf.lo.disable_ipv6 = 1\n".into(),
            permissions: Some("0644".into()),
        });

        let mut runcmd = String::new();
        // Interface naming is handled by netplan `match` + `set-name` above.
        // This script just applies addresses immediately for the first boot.
        let net_setup =
            Self::net_setup_script(primary_mac, mgmt_ip, cluster_ip.as_deref().zip(lan_mac))?;
        cloud_init_config.write_files.push(WriteFile {
            path: "/usr/local/bin/intar-net-setup.sh".into(),
            content: net_setup,
            permissions: Some("0755".into()),
        });
        runcmd.push_str("/usr/local/bin/intar-net-setup.sh\n");
        runcmd.push_str("cat /etc/hosts.intar >> /etc/hosts\n");
        if let Some(existing) = &cloud_init_config.runcmd {
            runcmd.push_str(existing);
        }
        cloud_init_config.runcmd = Some(runcmd);

        Ok(cloud_init_config)
    }

    fn next_port(&mut self) -> Result<u16, VmError> {
        let port = self
            .ports
            .get(self.port_index)
            .copied()
            .ok_or_else(|| VmError::Qemu("No available port".into()))?;
        self.port_index += 1;
        Ok(port)
    }

    #[cfg(unix)]
    fn host_socket_for_vm(&self, name: &str, suffix: &str) -> HostSocket {
        HostSocket::unix(self.work_dir.join(format!("{name}-{suffix}.sock")))
    }

    #[cfg(windows)]
    fn host_socket_for_vm(&mut self, _name: &str, _suffix: &str) -> Result<HostSocket, VmError> {
        let port = self.next_port()?;
        Ok(HostSocket::tcp(port))
    }

    fn base_image_for_vm(
        &self,
        vm_def: &VmDefinition,
        image_cache: &ImageCache,
        arch: &str,
    ) -> Result<PathBuf, VmError> {
        let image_spec = self.scenario.images.get(&vm_def.image).ok_or_else(|| {
            VmError::Qemu(format!(
                "Image '{}' not defined in scenario. Add an image block for it.",
                vm_def.image
            ))
        })?;

        let source = image_spec.source_for_arch(arch).ok_or_else(|| {
            VmError::Qemu(format!(
                "No image source for architecture '{}' in image '{}'",
                arch, vm_def.image
            ))
        })?;

        image_cache.get_cached_path(source).ok_or_else(|| {
            VmError::Qemu(format!(
                "Image '{}' not cached. Download it first.",
                vm_def.image
            ))
        })
    }

    fn agent_binary_for_arch(&self, arch: &str) -> Result<&Vec<u8>, VmError> {
        match arch {
            "x86_64" | "amd64" => Ok(&self.agent_binary_x86_64),
            "aarch64" | "arm64" => Ok(&self.agent_binary_aarch64),
            _ => Err(VmError::Qemu(format!("Unsupported architecture: {arch}"))),
        }
    }

    fn mgmt_ip(vm_index: usize) -> Result<String, VmError> {
        let idx = u32::try_from(vm_index)
            .map_err(|_| VmError::Qemu("Too many VMs for management IP addressing".into()))?;
        let last = 100u32
            .checked_add(idx)
            .filter(|octet| *octet <= 254)
            .ok_or_else(|| VmError::Qemu("Too many VMs for management IP addressing".into()))?;
        Ok(format!("10.0.2.{last}"))
    }

    fn netplan_config(
        primary_mac: &str,
        mgmt_ip: &str,
        lan: Option<(&str, &str)>,
    ) -> Result<String, VmError> {
        let mut netplan = format!(
            r#"network:
  version: 2
  ethernets:
    mgmt0:
      match:
        macaddress: "{primary_mac}"
      set-name: enp0s1
      dhcp4: false
      dhcp6: false
      addresses:
        - {mgmt_ip}/24
      gateway4: 10.0.2.2
      nameservers:
        addresses:
          - 10.0.2.3
      optional: true
"#
        );

        if let Some((cluster_ip, lan_mac)) = lan {
            write!(
                netplan,
                r#"    lan0:
      match:
        macaddress: "{lan_mac}"
      set-name: enp0s2
      dhcp4: false
      dhcp6: false
      addresses:
        - {cluster_ip}/24
      optional: true
"#,
            )
            .map_err(|_| VmError::Qemu("Failed to format network config".into()))?;
        }

        Ok(netplan)
    }

    fn net_setup_script(
        primary_mac: &str,
        mgmt_ip: &str,
        lan: Option<(&str, &str)>,
    ) -> Result<String, VmError> {
        let mut script = String::new();

        script.push_str(NET_SETUP_SCRIPT_PREFIX);
        write!(
            script,
            "PRIMARY_MAC=\"{primary_mac}\"\nLAN_MAC=\"\"\n\nMGMT_IF=\"$(iface_for_mac \"$PRIMARY_MAC\" || true)\"\nLAN_IF=\"\"\n",
        )
        .map_err(|_| VmError::Qemu("Failed to format network setup script".into()))?;

        if let Some((_cluster_ip, lan_mac)) = lan {
            write!(
                script,
                "LAN_MAC=\"{lan_mac}\"\nLAN_IF=\"$(iface_for_mac \"$LAN_MAC\" || true)\"\n",
            )
            .map_err(|_| VmError::Qemu("Failed to format network setup script".into()))?;
        }

        script.push_str(NET_SETUP_SCRIPT_RENAME_AND_FALLBACKS);

        script.push_str(
            r#"
# Configure management NIC immediately with static IPv4.
ip addr flush dev "$MGMT_IF" 2>/dev/null || true
"#,
        );
        writeln!(
            script,
            "ip addr add {mgmt_ip}/24 dev \"$MGMT_IF\" 2>/dev/null || true",
        )
        .map_err(|_| VmError::Qemu("Failed to format network setup script".into()))?;
        script.push_str(
            r#"ip link set "$MGMT_IF" up || true
ip route replace default via 10.0.2.2 dev "$MGMT_IF" 2>/dev/null || true
"#,
        );

        if let Some((cluster_ip, _lan_mac)) = lan {
            script.push_str(
                r#"
# Configure shared LAN NIC immediately with static IPv4.
ip addr flush dev "$LAN_IF" 2>/dev/null || true
"#,
            );
            writeln!(
                script,
                "ip addr add {cluster_ip}/24 dev \"$LAN_IF\" 2>/dev/null || true",
            )
            .map_err(|_| VmError::Qemu("Failed to format network setup script".into()))?;
            script.push_str(
                r#"ip link set "$LAN_IF" up || true
"#,
            );
        }

        script.push_str(
            r"
# Apply IPv6 disablement without blocking boot.
sysctl -p /etc/sysctl.d/99-intar-no-ipv6.conf 2>/dev/null || true
",
        );

        Ok(script)
    }

    fn assign_vm_addresses(scenario: &Scenario) -> Result<HashMap<String, String>, VmError> {
        let mut ips = HashMap::new();
        for (idx, vm) in scenario.vms.iter().enumerate() {
            let idx = u32::try_from(idx)
                .map_err(|_| VmError::Qemu("Too many VMs for shared LAN addressing".into()))?;
            let last = 10u32
                .checked_add(idx)
                .filter(|octet| *octet <= 254)
                .ok_or_else(|| VmError::Qemu("Too many VMs for shared LAN addressing".into()))?;
            ips.insert(vm.name.clone(), format!("10.11.0.{last}"));
        }
        Ok(ips)
    }

    fn render_hosts_file(&self) -> Result<String, VmError> {
        let mut content = String::from("127.0.0.1 localhost\n");

        for vm in &self.scenario.vms {
            if let Some(ip) = self.vm_addresses.get(&vm.name) {
                let mut names = vec![format!("{}.intar", vm.name), vm.name.clone()];
                if vm.name == "k3s-1" {
                    names.push("k3s-server.intar".into());
                    names.push("k3s-server".into());
                }
                writeln!(content, "{ip} {}", names.join(" "))
                    .map_err(|_| VmError::Qemu("Failed to format hosts file".into()))?;
            }
        }

        Ok(content)
    }

    fn generate_macs(idx: usize) -> Result<(String, Option<String>), VmError> {
        let idx = u8::try_from(idx)
            .map_err(|_| VmError::Qemu("Too many VMs to generate MAC addresses".into()))?;
        let primary_last = 0x10u8
            .checked_add(idx)
            .ok_or_else(|| VmError::Qemu("Too many VMs to generate MAC addresses".into()))?;
        let lan_last = 0x40u8
            .checked_add(idx)
            .ok_or_else(|| VmError::Qemu("Too many VMs to generate MAC addresses".into()))?;
        let primary = format!("52:54:00:12:56:{primary_last:02x}");
        let lan = format!("52:54:00:12:57:{lan_last:02x}");
        Ok((primary, Some(lan)))
    }

    fn start_lan_switch_if_needed(&mut self) -> Result<(), VmError> {
        let Some(hub_port) = self.shared_lan_hub_port else {
            return Ok(());
        };
        if self.lan_switch.is_some() {
            return Ok(());
        }

        let peers = self
            .vms
            .values()
            .filter_map(|vm| {
                vm.shared_lan
                    .as_ref()
                    .map(|SharedNetworkEndpoint::Dgram { local_port, .. }| {
                        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, *local_port))
                    })
            })
            .collect();

        self.lan_switch = Some(LanSwitch::spawn(hub_port, peers)?);
        Ok(())
    }

    /// Start all prepared VMs.
    ///
    /// # Errors
    /// Returns `VmError` if any VM fails to start or the state cannot be saved.
    pub fn start_vms(&mut self) -> Result<(), VmError> {
        self.start_lan_switch_if_needed()?;
        let arch = detect_arch();
        for name in &self.vm_order {
            if let Some(vm) = self.vms.get_mut(name) {
                vm.start(&arch)?;
                vm.state = VmState::CloudInit;
            }
        }
        self.save_state()?;
        Ok(())
    }

    /// Persist current VM metadata to disk.
    ///
    /// # Errors
    /// Returns `VmError` if writing the state file fails.
    pub fn save_state(&self) -> Result<(), VmError> {
        let state = RunState {
            scenario_name: self.scenario.name.clone(),
            vms: self
                .vms
                .values()
                .map(|vm| VmInfo {
                    name: vm.name.clone(),
                    ssh_port: vm.ssh_port,
                    image: vm.definition.image.clone(),
                })
                .collect(),
        };
        state.save(&self.work_dir)?;
        Ok(())
    }

    /// Wait for all guest agents to become responsive.
    ///
    /// # Errors
    /// Returns `VmError` if any agent fails or times out.
    pub async fn wait_for_agents(&mut self) -> Result<(), VmError> {
        for (name, vm) in &self.vms {
            info!("Waiting for agent on VM: {}", name);

            let result = timeout(Duration::from_secs(600), wait_for_agent(&vm.serial_socket)).await;

            match result {
                Ok(Ok(())) => {
                    info!("Agent ready on VM: {}", name);
                }
                Ok(Err(e)) => {
                    error!("Agent failed on VM {}: {}", name, e);
                    return Err(e);
                }
                Err(_) => {
                    error!("Timeout waiting for agent on VM: {}", name);
                    return Err(VmError::Timeout(format!("Agent timeout on {name}")));
                }
            }
        }

        for vm in self.vms.values_mut() {
            vm.state = VmState::Ready;
        }

        Ok(())
    }

    /// Dispatch probe checks to all VMs.
    ///
    /// # Errors
    /// Returns `VmError` if communication with agents fails.
    pub async fn check_probes(&mut self) -> Result<(), VmError> {
        self.check_probes_phase(ProbePhase::Scenario).await
    }

    /// Dispatch probe checks for a specific phase.
    async fn check_probes_phase(&mut self, phase: ProbePhase) -> Result<(), VmError> {
        for (vm_name, vm) in &self.vms {
            let probe_names = &self
                .scenario
                .vms
                .iter()
                .find(|v| v.name == *vm_name)
                .map(|v| v.probes.clone())
                .unwrap_or_default();

            let mut probes: Vec<(String, ProbeSpec)> = Vec::new();
            let mut probe_ids: Vec<String> = Vec::new();
            let mut local_failures: Vec<ProbeResult> = Vec::new();

            for name in probe_names {
                let Some(def) = self.scenario.probes.get(name) else {
                    local_failures.push(ProbeResult::fail(
                        name.clone(),
                        format!("Probe '{name}' not defined in scenario"),
                    ));
                    continue;
                };

                if def.phase != phase {
                    continue;
                }

                let config = def
                    .config
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                match ProbeSpec::from_definition(&def.probe_type, &config) {
                    Ok(spec) => {
                        probe_ids.push(name.clone());
                        probes.push((name.clone(), spec));
                    }
                    Err(e) => {
                        local_failures.push(ProbeResult::fail(
                            name.clone(),
                            format!("Invalid probe config: {e}"),
                        ));
                    }
                }
            }

            if let Some(vm_results) = self.probe_results.get_mut(vm_name) {
                for failure in local_failures {
                    vm_results.insert(failure.id.clone(), failure);
                }
            }

            if probes.is_empty() {
                continue;
            }

            match try_connect(&vm.serial_socket, 3, 500).await {
                Ok(mut conn) => match conn.check_all(probes).await {
                    Ok(results) => {
                        if let Some(vm_results) = self.probe_results.get_mut(vm_name) {
                            for result in results {
                                vm_results.insert(result.id.clone(), result);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to check probes on {}: {}", vm_name, e);
                        if let Some(vm_results) = self.probe_results.get_mut(vm_name) {
                            for id in probe_ids {
                                vm_results.entry(id.clone()).or_insert_with(|| {
                                    ProbeResult::fail(
                                        id,
                                        format!("Failed to check probes via agent: {e}"),
                                    )
                                });
                            }
                        }
                    }
                },
                Err(e) => {
                    warn!("Failed to connect to agent on {}: {}", vm_name, e);
                    if let Some(vm_results) = self.probe_results.get_mut(vm_name) {
                        for id in probe_ids {
                            vm_results.entry(id.clone()).or_insert_with(|| {
                                ProbeResult::fail(id, format!("Failed to connect to agent: {e}"))
                            });
                        }
                    }
                }
            }
        }

        if phase == ProbePhase::Scenario && self.all_scenario_probes_passing() {
            self.state = ScenarioState::Completed;
        }

        Ok(())
    }

    /// Wait for all boot probes to pass before entering Running phase.
    ///
    /// # Errors
    /// Returns `VmError` if probes cannot be evaluated or the wait times out.
    pub async fn wait_for_boot_probes(&mut self) -> Result<(), VmError> {
        // If there are no boot probes, return immediately.
        if self.total_boot_probe_count() == 0 {
            return Ok(());
        }

        for _ in 0..60 {
            self.check_probes_phase(ProbePhase::Boot).await?;
            if self.all_boot_probes_passing() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        Err(VmError::Timeout("Boot probes did not pass in time".into()))
    }

    #[must_use]
    pub fn all_scenario_probes_passing(&self) -> bool {
        for (vm_name, vm_results) in &self.probe_results {
            let expected_count = self
                .scenario
                .vms
                .iter()
                .find(|v| v.name == *vm_name)
                .map_or(0, |v| {
                    v.probes
                        .iter()
                        .filter(|p| {
                            self.scenario
                                .probes
                                .get(*p)
                                .is_some_and(|def| def.phase == ProbePhase::Scenario)
                        })
                        .count()
                });

            let passing_count = vm_results
                .values()
                .filter(|r| r.passed)
                .filter(|r| {
                    self.scenario
                        .probes
                        .get(&r.id)
                        .is_some_and(|def| def.phase == ProbePhase::Scenario)
                })
                .count();

            if passing_count != expected_count {
                return false;
            }
        }

        true
    }

    fn all_boot_probes_passing(&self) -> bool {
        for (vm_name, vm_results) in &self.probe_results {
            let expected_count = self
                .scenario
                .vms
                .iter()
                .find(|v| v.name == *vm_name)
                .map_or(0, |v| {
                    v.probes
                        .iter()
                        .filter(|p| {
                            self.scenario
                                .probes
                                .get(*p)
                                .is_some_and(|def| def.phase == ProbePhase::Boot)
                        })
                        .count()
                });

            let passing_count = vm_results
                .values()
                .filter(|r| r.passed)
                .filter(|r| {
                    self.scenario
                        .probes
                        .get(&r.id)
                        .is_some_and(|def| def.phase == ProbePhase::Boot)
                })
                .count();

            if passing_count != expected_count {
                return false;
            }
        }

        true
    }

    #[must_use]
    pub fn passing_probe_count(&self) -> usize {
        self.probe_results
            .values()
            .flat_map(|r| r.values())
            .filter(|r| r.passed)
            .filter(|r| {
                self.scenario
                    .probes
                    .get(&r.id)
                    .is_some_and(|def| def.phase == ProbePhase::Scenario)
            })
            .count()
    }

    #[must_use]
    pub fn passing_boot_probe_count(&self) -> usize {
        self.probe_results
            .values()
            .flat_map(|r| r.values())
            .filter(|r| r.passed)
            .filter(|r| {
                self.scenario
                    .probes
                    .get(&r.id)
                    .is_some_and(|def| def.phase == ProbePhase::Boot)
            })
            .count()
    }

    #[must_use]
    pub fn total_probe_count(&self) -> usize {
        self.scenario
            .vms
            .iter()
            .flat_map(|v| v.probes.iter())
            .filter(|p| {
                self.scenario
                    .probes
                    .get(*p)
                    .is_some_and(|def| def.phase == ProbePhase::Scenario)
            })
            .count()
    }

    #[must_use]
    pub fn total_boot_probe_count(&self) -> usize {
        self.scenario
            .vms
            .iter()
            .flat_map(|v| v.probes.iter())
            .filter(|p| {
                self.scenario
                    .probes
                    .get(*p)
                    .is_some_and(|def| def.phase == ProbePhase::Boot)
            })
            .count()
    }

    /// Create a full VM checkpoint (memory + disk).
    ///
    /// # Errors
    /// Returns `VmError` if any VM checkpoint command fails.
    pub async fn save_checkpoint(&self, name: &str) -> Result<(), VmError> {
        info!("Pausing all VMs for checkpoint '{}'", name);
        let pause_result = try_join_all(self.vms.values().map(QemuInstance::pause)).await;
        if let Err(e) = pause_result {
            let _ = try_join_all(self.vms.values().map(QemuInstance::resume)).await;
            return Err(e);
        }

        let snapshot_result = async {
            for (vm_name, vm) in &self.vms {
                info!("Saving checkpoint '{}' for VM: {}", name, vm_name);
                vm.save_checkpoint(name).await?;
            }
            Ok::<(), VmError>(())
        }
        .await;

        info!("Resuming all VMs after checkpoint '{}'", name);
        let resume_result = try_join_all(self.vms.values().map(QemuInstance::resume)).await;

        snapshot_result?;
        resume_result.map(|_| ())
    }

    /// Reset all VMs back to the initial checkpoint.
    ///
    /// # Errors
    /// Returns `VmError` if any VM fails to reset.
    pub async fn reset(&mut self) -> Result<(), VmError> {
        info!("Pausing all VMs for reset");
        let pause_result = try_join_all(self.vms.values().map(QemuInstance::pause)).await;
        if let Err(e) = pause_result {
            let _ = try_join_all(self.vms.values().map(QemuInstance::resume)).await;
            return Err(e);
        }

        let load_result = async {
            for (name, vm) in &self.vms {
                info!("Loading checkpoint 'init' for VM: {}", name);
                vm.load_checkpoint("init").await?;
            }
            Ok::<(), VmError>(())
        }
        .await;

        info!("Resuming all VMs after reset");
        let resume_result = try_join_all(self.vms.values().map(QemuInstance::resume)).await;

        load_result?;
        resume_result.map(|_| ())?;

        self.clear_probe_results();
        self.wait_for_agents().await?;
        self.wait_for_boot_probes().await?;
        self.state = ScenarioState::Running;

        Ok(())
    }

    fn clear_probe_results(&mut self) {
        self.probe_results.clear();
        for vm_name in self.vms.keys() {
            self.probe_results.insert(vm_name.clone(), HashMap::new());
        }
    }

    /// Stop all running VMs in this scenario.
    ///
    /// # Errors
    /// Returns `VmError` if stopping any VM fails.
    pub async fn stop(&mut self) -> Result<(), VmError> {
        info!("Stopping scenario: {}", self.scenario.name);

        for handle in self.action_tasks.drain(..) {
            handle.abort();
        }
        self.action_rx = None;

        for (name, vm) in &mut self.vms {
            info!("Stopping VM: {}", name);
            vm.stop().await?;
        }

        self.vms.clear();
        if let Some(mut switch) = self.lan_switch.take() {
            switch.stop();
        }

        Ok(())
    }

    /// Delete all on-disk artifacts for this run (logs, overlays, keys, state).
    ///
    /// # Errors
    /// Returns `VmError` if filesystem cleanup fails.
    pub fn cleanup(&self) -> Result<(), VmError> {
        if !self.work_dir.exists() {
            return Ok(());
        }

        for attempt in 0..5 {
            match std::fs::remove_dir_all(&self.work_dir) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt == 4 {
                        return Err(e.into());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn get_ssh_command(&self, vm_name: &str) -> Option<String> {
        self.vms.get(vm_name).map(|vm| {
            format!(
                "ssh -i {} -p {} -o StrictHostKeyChecking=no user@localhost",
                self.work_dir.join("id_ed25519").display(),
                vm.ssh_port
            )
        })
    }
}

impl Drop for ScenarioRunner {
    fn drop(&mut self) {
        if let Some(switch) = self.lan_switch.as_mut() {
            switch.stop();
        }
    }
}

async fn wait_for_agent(socket: &HostSocket) -> Result<(), VmError> {
    for _ in 0..60 {
        if let Ok(mut conn) = try_connect(socket, 1, 0).await
            && conn.ping().await.is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    Err(VmError::Timeout("Agent did not become ready".into()))
}

fn generate_ssh_keypair(work_dir: &Path) -> Result<(String, String), VmError> {
    let private_key_path = work_dir.join("id_ed25519");
    let public_key_path = work_dir.join("id_ed25519.pub");

    if private_key_path.exists() {
        std::fs::remove_file(&private_key_path)?;
    }
    if public_key_path.exists() {
        std::fs::remove_file(&public_key_path)?;
    }

    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            path_to_str(&private_key_path)?,
            "-N",
            "",
            "-q",
        ])
        .status()
        .map_err(VmError::Io)?;

    if !status.success() {
        return Err(VmError::Io(std::io::Error::other("ssh-keygen failed")));
    }

    let private_key = std::fs::read_to_string(&private_key_path)?;
    let public_key = std::fs::read_to_string(&public_key_path)?
        .trim()
        .to_string();

    Ok((private_key, public_key))
}

fn detect_arch() -> String {
    #[cfg(target_arch = "x86_64")]
    return "x86_64".to_string();

    #[cfg(target_arch = "aarch64")]
    return "aarch64".to_string();

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return std::env::consts::ARCH.to_string();
}
