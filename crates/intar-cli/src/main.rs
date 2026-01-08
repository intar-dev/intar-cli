mod agent;
mod commands;

use anyhow::Context;
use clap::{Parser, Subcommand};
use intar_vm::IntarDirs;
use std::path::PathBuf;
use tracing_appender::rolling;

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
    /// Show status of running scenario
    Status,
    /// SSH into a VM
    Ssh {
        /// Name of the VM
        vm_name: String,
        /// Name of the run (defaults to most recent)
        #[arg(short, long)]
        run: Option<String>,
    },
    /// Reset scenario to initial state
    Reset,
    /// Stop the running scenario
    Stop,
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
    let dirs = IntarDirs::new().context("Failed to initialize directories")?;
    dirs.ensure_dirs()
        .context("Failed to create intar directories")?;

    let log_dir = dirs.state.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = rolling::never(&log_dir, "intar.log");
    let (non_blocking, _log_guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_ansi(false)
        .with_writer(non_blocking)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { scenario } => {
            commands::start(scenario).await?;
        }
        Commands::Status => {
            commands::status()?;
        }
        Commands::Ssh { vm_name, run } => {
            commands::ssh(&vm_name, run.as_deref())?;
        }
        Commands::Reset => {
            commands::reset();
        }
        Commands::Stop => {
            commands::stop()?;
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
