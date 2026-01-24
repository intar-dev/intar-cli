use crate::{HostSocket, connect_host_socket};
use base64::Engine as _;
use intar_probes::ActionEvent;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
struct CastHeader {
    version: u8,
    width: u16,
    height: u16,
    timestamp: u64,
}

struct CastWriter {
    file: tokio::fs::File,
    start_ts_unix_ms: u64,
}

impl CastWriter {
    async fn start(
        path: PathBuf,
        start_ts_unix_ms: u64,
        width: u16,
        height: u16,
    ) -> std::io::Result<Self> {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await?;

        let header = CastHeader {
            version: 2,
            width,
            height,
            timestamp: start_ts_unix_ms / 1000,
        };
        let line = serde_json::to_string(&header)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;

        Ok(Self {
            file,
            start_ts_unix_ms,
        })
    }

    async fn write_event(
        &mut self,
        ts_unix_ms: u64,
        kind: &'static str,
        data: &str,
    ) -> std::io::Result<()> {
        let rel =
            Duration::from_millis(ts_unix_ms.saturating_sub(self.start_ts_unix_ms)).as_secs_f64();
        let line = serde_json::to_string(&(rel, kind, data))?;
        self.file.write_all(line.as_bytes()).await?;
        self.file.write_all(b"\n").await?;
        Ok(())
    }

    async fn finish(&mut self) -> std::io::Result<()> {
        self.file.flush().await
    }
}

#[derive(Default)]
struct LineCapture {
    input_line: String,
    input_escape: bool,
    output_line: String,
    output_escape: bool,
    prefer_raw: bool,
}

impl LineCapture {
    fn reset_buffers(&mut self) {
        self.input_line.clear();
        self.output_line.clear();
        self.input_escape = false;
        self.output_escape = false;
    }

    fn note_raw(&mut self) {
        self.prefer_raw = true;
    }

    fn flush_output(
        &mut self,
        received_at: Instant,
        vm_name: &str,
        tx_lines: &mpsc::Sender<ActionLineEvent>,
    ) {
        let trimmed = self.output_line.trim();
        if !trimmed.is_empty() && !is_prompt_line(trimmed) {
            let _ = tx_lines.try_send(ActionLineEvent {
                vm: vm_name.to_string(),
                received_at,
                line: trimmed.to_string(),
                kind: ActionLineKind::Output,
            });
        }
        self.output_line.clear();
    }
}

async fn handle_action_event(
    event: ActionEvent,
    received_at: Instant,
    vm_name: &str,
    tx_lines: &mpsc::Sender<ActionLineEvent>,
    log_dir: &Path,
    cast_writer: &mut Option<CastWriter>,
    line_state: &mut LineCapture,
) {
    match event {
        ActionEvent::SshCastStart {
            ts_unix_ms,
            width,
            height,
        } => {
            line_state.reset_buffers();
            line_state.note_raw();
            if let Some(mut writer) = cast_writer.take() {
                let _ = writer.finish().await;
            }
            let cast_path = log_dir.join(format!("ssh-session-{ts_unix_ms}.cast"));
            if let Ok(writer) = CastWriter::start(cast_path, ts_unix_ms, width, height).await {
                *cast_writer = Some(writer);
            }
        }
        ActionEvent::SshSessionStart { .. } => {
            line_state.reset_buffers();
        }
        ActionEvent::SshRawInput {
            ts_unix_ms,
            data_b64,
        } => {
            line_state.note_raw();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                if let Some(writer) = cast_writer.as_mut() {
                    let text = String::from_utf8_lossy(&bytes);
                    let _ = writer.write_event(ts_unix_ms, "i", &text).await;
                }
                derive_lines_from_input(
                    &bytes,
                    &mut line_state.input_line,
                    &mut line_state.input_escape,
                    received_at,
                    vm_name,
                    tx_lines,
                );
            }
        }
        ActionEvent::SshRawOutput {
            ts_unix_ms,
            data_b64,
        } => {
            line_state.note_raw();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                if let Some(writer) = cast_writer.as_mut() {
                    let text = String::from_utf8_lossy(&bytes);
                    let _ = writer.write_event(ts_unix_ms, "o", &text).await;
                }
                derive_lines_from_output(
                    &bytes,
                    &mut line_state.output_line,
                    &mut line_state.output_escape,
                    received_at,
                    vm_name,
                    tx_lines,
                );
            }
        }
        ActionEvent::SshLine { line, .. } => {
            if !line_state.prefer_raw {
                let _ = tx_lines.try_send(ActionLineEvent {
                    vm: vm_name.to_string(),
                    received_at,
                    line,
                    kind: ActionLineKind::Input,
                });
            }
        }
        ActionEvent::SshOutput { line, .. } => {
            if !line_state.prefer_raw {
                let _ = tx_lines.try_send(ActionLineEvent {
                    vm: vm_name.to_string(),
                    received_at,
                    line,
                    kind: ActionLineKind::Output,
                });
            }
        }
        ActionEvent::SshSessionEnd { .. } => {
            line_state.flush_output(received_at, vm_name, tx_lines);
            if let Some(mut writer) = cast_writer.take() {
                let _ = writer.finish().await;
            }
        }
        ActionEvent::Error { .. } => {}
    }
}

#[must_use]
pub fn start_vm_actions_task(
    vm_name: String,
    actions_socket: HostSocket,
    log_dir: PathBuf,
    tx_lines: mpsc::Sender<ActionLineEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok(stream) = connect_host_socket(&actions_socket).await else {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            };

            let _ = tokio::fs::create_dir_all(&log_dir).await;

            let mut reader = BufReader::new(stream);
            let mut cast_writer: Option<CastWriter> = None;
            let mut line_state = LineCapture::default();

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

                handle_action_event(
                    event,
                    received_at,
                    &vm_name,
                    &tx_lines,
                    &log_dir,
                    &mut cast_writer,
                    &mut line_state,
                )
                .await;
            }
        }
    })
}

fn derive_lines_from_input(
    chunk: &[u8],
    line: &mut String,
    in_escape: &mut bool,
    received_at: Instant,
    vm_name: &str,
    tx_lines: &mpsc::Sender<ActionLineEvent>,
) {
    for &b in chunk {
        if *in_escape {
            if (b as char).is_ascii_alphabetic() || b == b'~' {
                *in_escape = false;
            }
            continue;
        }

        match b {
            0x1b => {
                *in_escape = true;
            }
            b'\r' | b'\n' => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = tx_lines.try_send(ActionLineEvent {
                        vm: vm_name.to_string(),
                        received_at,
                        line: trimmed.to_string(),
                        kind: ActionLineKind::Input,
                    });
                }
                line.clear();
            }
            0x7f | 0x08 => {
                let _ = line.pop();
            }
            b'\t' => line.push('\t'),
            b if b.is_ascii_graphic() || b == b' ' => line.push(char::from(b)),
            _ => {}
        }
    }
}

fn derive_lines_from_output(
    chunk: &[u8],
    line: &mut String,
    in_escape: &mut bool,
    received_at: Instant,
    vm_name: &str,
    tx_lines: &mpsc::Sender<ActionLineEvent>,
) {
    for &b in chunk {
        if *in_escape {
            if (b as char).is_ascii_alphabetic() || b == b'~' {
                *in_escape = false;
            }
            continue;
        }

        match b {
            0x1b => {
                *in_escape = true;
            }
            b'\r' | b'\n' => {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !is_prompt_line(trimmed) {
                    let _ = tx_lines.try_send(ActionLineEvent {
                        vm: vm_name.to_string(),
                        received_at,
                        line: trimmed.to_string(),
                        kind: ActionLineKind::Output,
                    });
                }
                line.clear();
            }
            0x7f | 0x08 => {
                let _ = line.pop();
            }
            b'\t' => line.push('\t'),
            b if b.is_ascii_graphic() || b == b' ' => line.push(char::from(b)),
            _ => {}
        }
    }
}

fn is_prompt_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed == "$" || trimmed == "#" {
        return true;
    }

    let mut pos = trimmed.rfind('$');
    if pos.is_none() {
        pos = trimmed.rfind('#');
    }

    let Some(pos) = pos else {
        return false;
    };

    if !trimmed[pos + 1..].starts_with(' ') && !trimmed[pos + 1..].is_empty() {
        return false;
    }

    let prefix = &trimmed[..pos];
    prefix.contains('@') && prefix.contains(':')
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
