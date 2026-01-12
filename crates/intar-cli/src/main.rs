mod agent;
#[cfg(unix)]
mod commands_unix;
#[cfg(not(any(unix, windows)))]
mod commands_unix;
#[cfg(windows)]
mod commands_windows;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_appender::{non_blocking::WorkerGuard, rolling};

#[cfg(unix)]
use commands_unix as commands;
#[cfg(not(any(unix, windows)))]
use commands_unix as commands;
#[cfg(windows)]
use commands_windows as commands;

#[derive(Parser)]
#[command(name = "intar")]
#[command(about = "QEMU-based DevOps lab environment")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a scenario from an HCL file
    Start {
        /// Path to the scenario HCL file
        scenario: PathBuf,
    },
    /// Open an SSH session to a VM
    Ssh {
        /// Name of the VM
        vm_name: String,
        /// Name of the run (defaults to most recent)
        #[arg(short, long)]
        run: Option<String>,
        /// Run a command on the VM and exit
        #[arg(short, long)]
        command: Option<String>,
    },
    /// List available scenarios
    List {
        /// Directory to search for scenarios
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// View logs for a scenario run
    Logs {
        /// Name of the run (petname, e.g., "fluffy-tiger-1234")
        #[arg(short, long)]
        run: Option<String>,
        /// Name of the VM
        #[arg(short, long)]
        vm: Option<String>,
        /// Which log file to view (qemu, console, user-data, meta-data)
        #[arg(short = 't', long, default_value = "console")]
        log_type: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _log_guard = init_logging();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { scenario } => {
            commands::start(scenario).await?;
        }
        Commands::Ssh {
            vm_name,
            run,
            command,
        } => {
            commands::ssh(&vm_name, run.as_deref(), command.as_deref())?;
        }
        Commands::List { dir } => {
            commands::list(&dir)?;
        }
        Commands::Logs { run, vm, log_type } => {
            commands::logs(run.as_deref(), vm.as_deref(), &log_type)?;
        }
    }

    Ok(())
}

fn intar_log_dir() -> anyhow::Result<std::path::PathBuf> {
    let state_dir = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .ok_or_else(|| anyhow::anyhow!("state directory not found"))?;
    Ok(state_dir.join("intar").join("logs"))
}

fn init_logging() -> Option<WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::Level::INFO.into());

    if let Ok(log_dir) = intar_log_dir()
        && std::fs::create_dir_all(&log_dir).is_ok()
    {
        let log_path = log_dir.join("intar.log");
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .is_ok()
        {
            let file_appender = rolling::never(&log_dir, "intar.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_ansi(false)
                .with_writer(non_blocking)
                .init();
            return Some(guard);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    None
}
