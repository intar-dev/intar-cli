use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionEvent {
    SshCastStart {
        ts_unix_ms: u64,
        width: u16,
        height: u16,
    },
    SshSessionStart {
        ts_unix_ms: u64,
        user: String,
        kind: SshSessionKind,
    },
    SshRawInput {
        ts_unix_ms: u64,
        data_b64: String,
    },
    SshRawOutput {
        ts_unix_ms: u64,
        data_b64: String,
    },
    SshLine {
        ts_unix_ms: u64,
        line: String,
    },
    SshOutput {
        ts_unix_ms: u64,
        line: String,
    },
    SshSessionEnd {
        ts_unix_ms: u64,
        exit_code: i32,
    },
    Error {
        ts_unix_ms: u64,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshSessionKind {
    Interactive,
    Command,
}
