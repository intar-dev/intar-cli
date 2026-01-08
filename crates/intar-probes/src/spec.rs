use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeSpec {
    FileContent {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
    },
    FileExists {
        path: String,
        exists: bool,
    },
    Service {
        service: String,
        state: ServiceState,
    },
    Port {
        port: u16,
        state: PortState,
        #[serde(default = "default_protocol")]
        protocol: Protocol,
    },
    Command {
        cmd: String,
        exit_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        stdout_contains: Option<String>,
    },
    Http {
        url: String,
        status: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_contains: Option<String>,
    },
    K8sNodesReady {
        expected_ready: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        kubeconfig: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    #[serde(alias = "k8s_endpoints_nonempty")]
    K8sEndpointsNonEmpty {
        namespace: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        kubeconfig: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    TcpPing {
        host: String,
        #[serde(default = "default_tcp_ping_port")]
        port: u16,
        #[serde(default = "default_tcp_ping_timeout_ms")]
        timeout_ms: u64,
        #[serde(default = "default_tcp_ping_state")]
        state: ReachabilityState,
    },
}

fn default_protocol() -> Protocol {
    Protocol::Tcp
}

fn default_tcp_ping_port() -> u16 {
    1
}

fn default_tcp_ping_timeout_ms() -> u64 {
    2000
}

fn default_tcp_ping_state() -> ReachabilityState {
    ReachabilityState::Reachable
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Stopped,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortState {
    Listening,
    Closed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityState {
    Reachable,
    Unreachable,
}

impl ProbeSpec {
    /// Construct a `ProbeSpec` from a probe type string and config map.
    ///
    /// # Errors
    /// Returns an error string when the combined map cannot be deserialized into a `ProbeSpec`.
    pub fn from_definition(
        probe_type: &str,
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        let mut full_config = config.clone();
        full_config.insert(
            "type".to_string(),
            serde_json::Value::String(probe_type.to_string()),
        );

        serde_json::from_value(serde_json::Value::Object(full_config))
            .map_err(|e| format!("Failed to parse probe config: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_spec_serialization() {
        let spec = ProbeSpec::Service {
            service: "nginx".to_string(),
            state: ServiceState::Running,
        };

        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("service"));
        assert!(json.contains("nginx"));

        let parsed: ProbeSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }

    #[test]
    fn test_from_definition() {
        let mut config = serde_json::Map::new();
        config.insert(
            "service".to_string(),
            serde_json::Value::String("nginx".to_string()),
        );
        config.insert(
            "state".to_string(),
            serde_json::Value::String("running".to_string()),
        );

        let spec = ProbeSpec::from_definition("service", &config).unwrap();
        assert!(matches!(spec, ProbeSpec::Service { .. }));
    }

    #[test]
    fn test_from_definition_alias_k8s_endpoints_nonempty() {
        let mut config = serde_json::Map::new();
        config.insert(
            "namespace".to_string(),
            serde_json::Value::String("default".to_string()),
        );
        config.insert(
            "name".to_string(),
            serde_json::Value::String("echo-svc".to_string()),
        );

        let spec = ProbeSpec::from_definition("k8s_endpoints_nonempty", &config).unwrap();
        assert!(matches!(spec, ProbeSpec::K8sEndpointsNonEmpty { .. }));
    }
}
