use crate::{HostSocket, connect_host_socket};
use intar_probes::ActionEvent;
use serde::Serialize;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct ActionLineEvent {
    pub vm: String,
    pub received_at: Instant,
    pub line: String,
    pub kind: ActionLineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionLineKind {
    Input,
    Output,
}

#[derive(Debug, Serialize)]
struct ActionLogRecord<'a> {
    received_unix_ms: u64,
    vm: &'a str,
    event: ActionEvent,
}

#[must_use]
pub fn start_vm_actions_task(
    vm_name: String,
    actions_socket: HostSocket,
    log_path: PathBuf,
    tx_lines: mpsc::Sender<ActionLineEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok(stream) = connect_host_socket(&actions_socket).await else {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            };

            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await;
            let Ok(mut file) = file else { return };

            let mut reader = BufReader::new(stream);

            loop {
                let mut line = String::new();
                let Ok(bytes) = reader.read_line(&mut line).await else {
                    break;
                };
                if bytes == 0 {
                    break;
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let received_at = Instant::now();
                let received_unix_ms = unix_ms();

                let event = match serde_json::from_str::<ActionEvent>(trimmed) {
                    Ok(e) => e,
                    Err(e) => ActionEvent::Error {
                        ts_unix_ms: received_unix_ms,
                        message: format!("Failed to parse action event: {e}"),
                    },
                };

                let record = ActionLogRecord {
                    received_unix_ms,
                    vm: &vm_name,
                    event: event.clone(),
                };

                if let Ok(json) = serde_json::to_string(&record) {
                    let _ = file.write_all(json.as_bytes()).await;
                    let _ = file.write_all(b"\n").await;
                    let _ = file.flush().await;
                }

                match event {
                    ActionEvent::SshLine { line, .. } => {
                        let _ = tx_lines.try_send(ActionLineEvent {
                            vm: vm_name.clone(),
                            received_at,
                            line,
                            kind: ActionLineKind::Input,
                        });
                    }
                    ActionEvent::SshOutput { line, .. } => {
                        let _ = tx_lines.try_send(ActionLineEvent {
                            vm: vm_name.clone(),
                            received_at,
                            line,
                            kind: ActionLineKind::Output,
                        });
                    }
                    _ => {}
                }
            }
        }
    })
}

fn unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
