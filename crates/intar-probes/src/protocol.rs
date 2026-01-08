use crate::ProbeSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CheckProbe { id: String, spec: ProbeSpec },
    CheckAll { probes: Vec<(String, ProbeSpec)> },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    ProbeResult {
        id: String,
        passed: bool,
        message: String,
    },
    AllResults {
        results: Vec<ProbeResult>,
    },
    Pong {
        uptime_secs: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub id: String,
    pub passed: bool,
    pub message: String,
}

impl ProbeResult {
    pub fn pass(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            passed: true,
            message: message.into(),
        }
    }

    pub fn fail(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            passed: false,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = Request::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("ping"));

        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, Request::Ping));
    }

    #[test]
    fn test_response_serialization() {
        let resp = Response::Pong { uptime_secs: 42 };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("pong"));
        assert!(json.contains("42"));
    }
}
