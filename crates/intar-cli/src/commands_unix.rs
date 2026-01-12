use crate::agent::{AGENT_AARCH64, AGENT_X86_64, is_placeholder};
use anyhow::{Context, Result, bail};
use intar_core::Scenario;
use intar_ui::App;
use intar_vm::IntarDirs;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub async fn start(scenario_path: PathBuf) -> Result<()> {
    if is_placeholder(AGENT_X86_64) || is_placeholder(AGENT_AARCH64) {
        bail!(
            "Agent binaries not built. Please install cargo-zigbuild and zig:\n\
            \n\
            cargo install cargo-zigbuild\n\
            brew install zig  # or appropriate package manager\n\
            \n\
            Then rebuild (debug is fine) with: cargo build -p intar-cli\n\
            or simply re-run: cargo run --bin intar -- start <scenario.hcl>"
        );
    }

    let scenario = Scenario::from_file(&scenario_path).context("Failed to parse scenario")?;

    scenario.validate().context("Scenario validation failed")?;

    let mut app = App::new(scenario, AGENT_X86_64.to_vec(), AGENT_AARCH64.to_vec());
    app.run().await?;

    Ok(())
}

pub fn ssh(vm_name: &str, run_name: Option<&str>, command: Option<&str>) -> Result<()> {
    let dirs = IntarDirs::new().context("Failed to initialize directories")?;
    let runs_root = dirs.runs_dir();

    let run_dir = if let Some(name) = run_name {
        let dir = runs_root.join(name);
        if !dir.exists() {
            bail!("Run '{}' not found in {}", name, runs_root.display());
        }
        if !dir.join("state.json").exists() {
            bail!("Run '{name}' has no state file");
        }
        dir
    } else {
        let mut entries: Vec<_> = std::fs::read_dir(&runs_root)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter(|e| e.path().join("state.json").exists())
            .collect();

        if entries.is_empty() {
            bail!("No running scenario found. Start one with: intar start <scenario.hcl>");
        }

        entries.sort_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        entries.pop().unwrap().path()
    };
    let state = intar_vm::RunState::load(&run_dir).context("Failed to load run state")?;

    let vm_info = state
        .vms
        .iter()
        .find(|vm| vm.name == vm_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "VM '{}' not found. Available VMs: {}",
                vm_name,
                state
                    .vms
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    let ssh_key = run_dir.join("id_ed25519");
    if !ssh_key.exists() {
        bail!("SSH key not found at {}", ssh_key.display());
    }

    let mut cmd = std::process::Command::new("ssh");
    cmd.args([
        "-i",
        &ssh_key.to_string_lossy(),
        "-p",
        &vm_info.ssh_port.to_string(),
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "LogLevel=ERROR",
        "user@localhost",
    ]);

    if let Some(command) = command {
        cmd.arg(command);
    }

    let status = cmd.status().context("Failed to execute ssh")?;

    if !status.success() {
        bail!("SSH exited with status: {status}");
    }

    Ok(())
}

pub fn list(dir: &Path) -> Result<()> {
    println!("Searching for scenarios in: {}", dir.display());

    let entries: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "hcl"))
        .map(walkdir::DirEntry::into_path)
        .collect();

    if entries.is_empty() {
        println!("No .hcl scenario files found.");
        return Ok(());
    }

    for path in entries {
        if let Ok(scenario) = Scenario::from_file(&path) {
            println!("  {} - {}", scenario.name, scenario.description);
            println!("    File: {}", path.display());
            println!("    VMs: {}", scenario.vms.len());
            println!("    Probes: {}", scenario.total_probe_count());
            println!();
        }
    }

    Ok(())
}

pub fn logs(run_name: Option<&str>, vm_name: Option<&str>, log_type: &str) -> Result<()> {
    let dirs = IntarDirs::new().context("Failed to initialize directories")?;
    let runs_root = dirs.runs_dir();

    let run_dir = if let Some(name) = run_name {
        let dir = runs_root.join(name);
        if !dir.exists() {
            bail!("Run '{}' not found in {}", name, runs_root.display());
        }
        dir
    } else {
        let mut entries: Vec<_> = std::fs::read_dir(&runs_root)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        if entries.is_empty() {
            bail!("No scenario runs found in {}", runs_root.display());
        }

        entries.sort_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        entries.pop().unwrap().path()
    };

    let logs_dir = run_dir.join("logs");
    if !logs_dir.exists() {
        bail!("No logs directory found in {}", run_dir.display());
    }

    let vm_dir = if let Some(name) = vm_name {
        let dir = logs_dir.join(name);
        if !dir.exists() {
            bail!("VM '{}' not found in {}", name, logs_dir.display());
        }
        dir
    } else {
        let first_vm = std::fs::read_dir(&logs_dir)?
            .filter_map(Result::ok)
            .find(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));

        match first_vm {
            Some(entry) => entry.path(),
            None => bail!("No VM logs found in {}", logs_dir.display()),
        }
    };

    let log_file = match log_type {
        "qemu" => vm_dir.join("qemu.log"),
        "console" => vm_dir.join("console.log"),
        "user-data" => vm_dir.join("user-data.yaml"),
        "meta-data" => vm_dir.join("meta-data.yaml"),
        other => bail!("Unknown log type '{other}'. Use: qemu, console, user-data, meta-data",),
    };

    if !log_file.exists() {
        bail!("Log file not found: {}", log_file.display());
    }

    let header = format!("=== {} ===\n\n", log_file.display());
    write_stdout_all(header.as_bytes())?;

    let file = File::open(&log_file)?;
    copy_to_stdout(file)?;

    Ok(())
}

fn write_stdout_all(bytes: &[u8]) -> Result<()> {
    let mut stdout = io::stdout();
    if let Err(e) = stdout.write_all(bytes) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e.into());
    }
    Ok(())
}

fn copy_to_stdout(mut file: File) -> Result<()> {
    let mut stdout = io::stdout();
    match io::copy(&mut file, &mut stdout) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}
