use crate::{PortState, ProbeResult, ProbeSpec, Protocol, ServiceState};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

#[must_use]
pub fn evaluate_probe(id: &str, spec: &ProbeSpec) -> ProbeResult {
    match evaluate_probe_inner(spec) {
        Ok(message) => ProbeResult::pass(id, message),
        Err(message) => ProbeResult::fail(id, message),
    }
}

fn evaluate_probe_inner(spec: &ProbeSpec) -> Result<String, String> {
    match spec {
        ProbeSpec::FileContent {
            path,
            contains,
            regex,
        } => eval_file_content(path, contains.as_deref(), regex.as_deref()),
        ProbeSpec::FileExists { path, exists } => eval_file_exists(path, *exists),
        ProbeSpec::Service { service, state } => eval_service(service, *state),
        ProbeSpec::Port {
            port,
            state,
            protocol,
        } => eval_port(*port, *state, *protocol),
        ProbeSpec::Command {
            cmd,
            exit_code,
            stdout_contains,
        } => eval_command(cmd, *exit_code, stdout_contains.as_deref()),
        ProbeSpec::Http {
            url,
            status,
            body_contains,
        } => eval_http(url, *status, body_contains.as_deref()),
        ProbeSpec::K8sNodesReady {
            expected_ready,
            kubeconfig,
            context,
        } => eval_k8s_nodes_ready(*expected_ready, kubeconfig.as_deref(), context.as_deref()),
        ProbeSpec::K8sEndpointsNonEmpty {
            namespace,
            name,
            kubeconfig,
            context,
        } => {
            eval_k8s_endpoints_nonempty(namespace, name, kubeconfig.as_deref(), context.as_deref())
        }
        ProbeSpec::TcpPing {
            host,
            port,
            timeout_ms,
            state,
        } => eval_tcp_ping(host, *port, Duration::from_millis(*timeout_ms), *state),
    }
}

fn eval_file_content(
    path: &str,
    contains: Option<&str>,
    regex_pattern: Option<&str>,
) -> Result<String, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read file '{path}': {e}"))?;

    if let Some(needle) = contains
        && !content.contains(needle)
    {
        return Err(format!("File '{path}' does not contain '{needle}'"));
    }

    if let Some(pattern) = regex_pattern {
        let re =
            regex::Regex::new(pattern).map_err(|e| format!("Invalid regex '{pattern}': {e}"))?;
        if !re.is_match(&content) {
            return Err(format!("File '{path}' does not match regex '{pattern}'"));
        }
    }

    Ok(format!("File '{path}' content matches criteria"))
}

fn eval_file_exists(path: &str, should_exist: bool) -> Result<String, String> {
    let exists = std::path::Path::new(path).exists();

    if exists == should_exist {
        if should_exist {
            Ok(format!("File '{path}' exists"))
        } else {
            Ok(format!("File '{path}' does not exist"))
        }
    } else if should_exist {
        Err(format!("File '{path}' does not exist"))
    } else {
        Err(format!("File '{path}' exists but should not"))
    }
}

fn eval_service(service: &str, expected_state: ServiceState) -> Result<String, String> {
    match expected_state {
        ServiceState::Running | ServiceState::Stopped => {
            let output = Command::new("systemctl")
                .args(["is-active", service])
                .output()
                .map_err(|e| format!("Failed to check service '{service}': {e}"))?;

            let is_active = output.status.success();
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();

            match (expected_state, is_active) {
                (ServiceState::Running, true) => Ok(format!("Service '{service}' is {status}")),
                (ServiceState::Running, false) => {
                    Err(format!("Service '{service}' is not running ({status})"))
                }
                (ServiceState::Stopped, false) => Ok(format!("Service '{service}' is stopped")),
                (ServiceState::Stopped, true) => Err(format!(
                    "Service '{service}' is running but should be stopped"
                )),
                _ => unreachable!(),
            }
        }
        ServiceState::Enabled | ServiceState::Disabled => {
            let output = Command::new("systemctl")
                .args(["is-enabled", service])
                .output()
                .map_err(|e| format!("Failed to check service '{service}': {e}"))?;

            let is_enabled = output.status.success();
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();

            match (expected_state, is_enabled) {
                (ServiceState::Enabled, true) => Ok(format!("Service '{service}' is {status}")),
                (ServiceState::Enabled, false) => {
                    Err(format!("Service '{service}' is not enabled ({status})"))
                }
                (ServiceState::Disabled, false) => Ok(format!("Service '{service}' is disabled")),
                (ServiceState::Disabled, true) => Err(format!(
                    "Service '{service}' is enabled but should be disabled"
                )),
                _ => unreachable!(),
            }
        }
    }
}

fn eval_port(port: u16, expected_state: PortState, protocol: Protocol) -> Result<String, String> {
    let is_listening = tokio_runtime()?
        .block_on(is_port_listening(port, protocol))
        .map_err(|e| format!("Failed to check port {port}: {e}"))?;

    match (expected_state, is_listening) {
        (PortState::Listening, true) => Ok(format!("Port {port} is listening")),
        (PortState::Listening, false) => Err(format!("Port {port} is not listening")),
        (PortState::Closed, false) => Ok(format!("Port {port} is closed")),
        (PortState::Closed, true) => Err(format!("Port {port} is listening but should be closed")),
    }
}

fn tokio_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))
    });

    runtime.as_ref().map_err(Clone::clone)
}

async fn is_port_listening(port: u16, protocol: Protocol) -> Result<bool, String> {
    match protocol {
        Protocol::Tcp => is_tcp_port_listening(port).await,
        Protocol::Udp => is_udp_port_listening(port).await,
    }
}

async fn is_tcp_port_listening(port: u16) -> Result<bool, String> {
    let addr_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let addr_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);

    let (v4, v6) = tokio::join!(
        tcp_connect_attempt(addr_v4, Duration::from_millis(500)),
        tcp_connect_attempt(addr_v6, Duration::from_millis(500))
    );

    let v4 = v4?;
    let v6 = v6?;

    let mut attempted_any = false;

    for attempt in [v4, v6] {
        match attempt {
            PortReachability::Listening => return Ok(true),
            PortReachability::Closed => attempted_any = true,
            PortReachability::AddressUnavailable => {}
        }
    }

    if attempted_any {
        Ok(false)
    } else {
        Err("no loopback address available".to_string())
    }
}

async fn is_udp_port_listening(port: u16) -> Result<bool, String> {
    let addr_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let addr_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);

    let (v4, v6) = tokio::join!(udp_bind_attempt(addr_v4), udp_bind_attempt(addr_v6));

    let v4 = v4?;
    let v6 = v6?;

    let mut attempted_any = false;

    for attempt in [v4, v6] {
        match attempt {
            PortReachability::Listening => return Ok(true),
            PortReachability::Closed => attempted_any = true,
            PortReachability::AddressUnavailable => {}
        }
    }

    if attempted_any {
        Ok(false)
    } else {
        Err("no local address available".to_string())
    }
}

#[derive(Debug, Clone, Copy)]
enum PortReachability {
    Listening,
    Closed,
    AddressUnavailable,
}

async fn tcp_connect_attempt(
    addr: SocketAddr,
    timeout: Duration,
) -> Result<PortReachability, String> {
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(PortReachability::Listening)
        }
        Ok(Err(err)) => Ok(classify_port_error(&err, "TCP connect to", addr)?),
        Err(_) => Ok(PortReachability::Closed),
    }
}

async fn udp_bind_attempt(addr: SocketAddr) -> Result<PortReachability, String> {
    match tokio::net::UdpSocket::bind(addr).await {
        Ok(socket) => {
            drop(socket);
            Ok(PortReachability::Closed)
        }
        Err(err) => Ok(classify_port_error(&err, "UDP bind to", addr)?),
    }
}

fn classify_port_error(
    err: &io::Error,
    operation: &'static str,
    addr: SocketAddr,
) -> Result<PortReachability, String> {
    match err.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut => Ok(PortReachability::Closed),
        io::ErrorKind::AddrInUse => Ok(PortReachability::Listening),
        io::ErrorKind::AddrNotAvailable
        | io::ErrorKind::NetworkUnreachable
        | io::ErrorKind::InvalidInput => Ok(PortReachability::AddressUnavailable),
        _ => Err(format!("{operation} {addr} failed: {err}")),
    }
}

fn eval_tcp_ping(
    host: &str,
    port: u16,
    timeout: Duration,
    expected_state: crate::ReachabilityState,
) -> Result<String, String> {
    let reachable = tokio_runtime()?
        .block_on(tcp_reachable(host, port, timeout))
        .map_err(|e| format!("Failed to check reachability for {host}:{port}: {e}"))?;

    match (expected_state, reachable) {
        (crate::ReachabilityState::Reachable, true) => Ok(format!("{host} is reachable")),
        (crate::ReachabilityState::Reachable, false) => Err(format!("{host} is not reachable")),
        (crate::ReachabilityState::Unreachable, false) => Ok(format!("{host} is not reachable")),
        (crate::ReachabilityState::Unreachable, true) => {
            Err(format!("{host} is reachable but should not be"))
        }
    }
}

async fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> Result<bool, String> {
    let addrs = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| format!("Timed out resolving '{host}'"))?
        .map_err(|e| format!("Failed to resolve '{host}': {e}"))?
        .collect::<Vec<_>>();

    if addrs.is_empty() {
        return Err(format!("No addresses found for '{host}'"));
    }

    let mut attempted_any = false;

    for addr in addrs {
        match tcp_reachability_attempt(addr, timeout).await? {
            ReachabilityAttempt::Reachable => return Ok(true),
            ReachabilityAttempt::Unreachable => attempted_any = true,
            ReachabilityAttempt::AddressUnavailable => {}
        }
    }

    if attempted_any {
        Ok(false)
    } else {
        Err("no local address available".to_string())
    }
}

#[derive(Debug, Clone, Copy)]
enum ReachabilityAttempt {
    Reachable,
    Unreachable,
    AddressUnavailable,
}

async fn tcp_reachability_attempt(
    addr: SocketAddr,
    timeout: Duration,
) -> Result<ReachabilityAttempt, String> {
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(ReachabilityAttempt::Reachable)
        }
        Ok(Err(err)) => Ok(classify_reachability_error(&err)?),
        Err(_) => Ok(ReachabilityAttempt::Unreachable),
    }
}

fn classify_reachability_error(err: &io::Error) -> Result<ReachabilityAttempt, String> {
    match err.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset => {
            Ok(ReachabilityAttempt::Reachable)
        }
        io::ErrorKind::TimedOut
        | io::ErrorKind::HostUnreachable
        | io::ErrorKind::NetworkUnreachable => Ok(ReachabilityAttempt::Unreachable),
        io::ErrorKind::AddrNotAvailable
        | io::ErrorKind::InvalidInput
        | io::ErrorKind::Unsupported => Ok(ReachabilityAttempt::AddressUnavailable),
        _ => Err(format!("TCP connect failed: {err}")),
    }
}

fn eval_k8s_nodes_ready(
    expected_ready: u32,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<String, String> {
    #[cfg(feature = "kubernetes")]
    {
        tokio_runtime()?.block_on(k8s_nodes_ready(expected_ready, kubeconfig, context))
    }

    #[cfg(not(feature = "kubernetes"))]
    {
        let _ = (expected_ready, kubeconfig, context);
        Err("Kubernetes probes are not supported in this build".to_string())
    }
}

fn eval_k8s_endpoints_nonempty(
    namespace: &str,
    name: &str,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<String, String> {
    #[cfg(feature = "kubernetes")]
    {
        tokio_runtime()?.block_on(k8s_endpoints_nonempty(namespace, name, kubeconfig, context))
    }

    #[cfg(not(feature = "kubernetes"))]
    {
        let _ = (namespace, name, kubeconfig, context);
        Err("Kubernetes probes are not supported in this build".to_string())
    }
}

#[cfg(feature = "kubernetes")]
async fn k8s_nodes_ready(
    expected_ready: u32,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<String, String> {
    use k8s_openapi::api::core::v1::Node;
    use kube::api::{Api, ListParams};

    let client = k8s_client(kubeconfig, context).await?;
    let nodes: Api<Node> = Api::all(client);

    let list = tokio::time::timeout(Duration::from_secs(5), nodes.list(&ListParams::default()))
        .await
        .map_err(|_| "Timed out listing nodes".to_string())?
        .map_err(|e| format!("Failed to list nodes: {e}"))?;

    let ready = u32::try_from(list.items.iter().filter(|node| node_is_ready(node)).count())
        .map_err(|_| "Node count does not fit in u32".to_string())?;

    if ready == expected_ready {
        Ok(format!("{ready}/{expected_ready} nodes are Ready"))
    } else {
        Err(format!("Only {ready}/{expected_ready} nodes are Ready"))
    }
}

#[cfg(feature = "kubernetes")]
async fn k8s_endpoints_nonempty(
    namespace: &str,
    name: &str,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<String, String> {
    use k8s_openapi::api::core::v1::Endpoints;
    use k8s_openapi::api::discovery::v1::EndpointSlice;
    use kube::api::{Api, ListParams};

    let client = k8s_client(kubeconfig, context).await?;
    let endpoints: Api<Endpoints> = Api::namespaced(client.clone(), namespace);
    let endpoint_slices: Api<EndpointSlice> = Api::namespaced(client, namespace);

    let ep = match tokio::time::timeout(Duration::from_secs(5), endpoints.get(name)).await {
        Err(_) => return Err(format!("Timed out fetching Endpoints '{namespace}/{name}'")),
        Ok(Ok(ep)) => Some(ep),
        Ok(Err(kube::Error::Api(err))) if err.code == 404 => None,
        Ok(Err(e)) => {
            return Err(format!(
                "Failed to fetch Endpoints '{namespace}/{name}': {e}"
            ));
        }
    };

    if ep.is_some_and(|ep| endpoints_have_addresses(&ep)) {
        return Ok(format!(
            "Service '{namespace}/{name}' has endpoints (Endpoints)"
        ));
    }

    let label_selector = format!("kubernetes.io/service-name={name}");
    let slices = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint_slices.list(&ListParams::default().labels(&label_selector)),
    )
    .await
    .map_err(|_| format!("Timed out listing EndpointSlices for '{namespace}/{name}'"))?
    .map_err(|e| format!("Failed to list EndpointSlices for '{namespace}/{name}': {e}"))?;

    let address_count: usize = slices
        .items
        .iter()
        .map(|slice| {
            slice
                .endpoints
                .iter()
                .map(|ep| ep.addresses.len())
                .sum::<usize>()
        })
        .sum();

    if address_count > 0 {
        Ok(format!(
            "Service '{namespace}/{name}' has endpoints (EndpointSlices: {address_count} addresses)"
        ))
    } else {
        Err(format!("Service '{namespace}/{name}' has no endpoints"))
    }
}

#[cfg(feature = "kubernetes")]
fn endpoints_have_addresses(ep: &k8s_openapi::api::core::v1::Endpoints) -> bool {
    ep.subsets.as_ref().is_some_and(|subsets| {
        subsets.iter().any(|subset| {
            subset
                .addresses
                .as_ref()
                .is_some_and(|addresses| !addresses.is_empty())
                || subset
                    .not_ready_addresses
                    .as_ref()
                    .is_some_and(|addresses| !addresses.is_empty())
        })
    })
}

#[cfg(feature = "kubernetes")]
async fn k8s_client(
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<kube::Client, String> {
    use kube::config::{KubeConfigOptions, Kubeconfig};
    use kube::{Client, Config};

    let kubeconfig = kubeconfig
        .map(str::to_string)
        .or_else(find_default_kubeconfig);

    if let Some(kubeconfig) = kubeconfig {
        let config = Kubeconfig::read_from(&kubeconfig)
            .map_err(|e| format!("Failed to read kubeconfig '{kubeconfig}': {e}"))?;

        let options = KubeConfigOptions {
            context: context.map(str::to_string),
            ..Default::default()
        };

        let config = Config::from_custom_kubeconfig(config, &options)
            .await
            .map_err(|e| format!("Failed to load kubeconfig '{kubeconfig}': {e}"))?;

        return Client::try_from(config)
            .map_err(|e| format!("Failed to create Kubernetes client: {e}"));
    }

    if context.is_some() {
        let options = KubeConfigOptions {
            context: context.map(str::to_string),
            ..Default::default()
        };

        let config = Config::from_kubeconfig(&options)
            .await
            .map_err(|e| format!("Failed to load kubeconfig: {e}"))?;

        return Client::try_from(config)
            .map_err(|e| format!("Failed to create Kubernetes client: {e}"));
    }

    Client::try_default()
        .await
        .map_err(|e| format!("Failed to infer Kubernetes client config: {e}"))
}

#[cfg(feature = "kubernetes")]
fn find_default_kubeconfig() -> Option<String> {
    ["/etc/rancher/k3s/k3s.yaml", "/etc/kubernetes/admin.conf"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .map(ToString::to_string)
}

#[cfg(feature = "kubernetes")]
fn node_is_ready(node: &k8s_openapi::api::core::v1::Node) -> bool {
    node.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

fn eval_command(
    cmd: &str,
    expected_exit_code: i32,
    stdout_contains: Option<&str>,
) -> Result<String, String> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| format!("Failed to execute command: {e}"))?;

    let actual_exit_code = output.status.code().unwrap_or(-1);

    if actual_exit_code != expected_exit_code {
        return Err(format!(
            "Command exited with code {actual_exit_code} (expected {expected_exit_code})"
        ));
    }

    if let Some(needle) = stdout_contains {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains(needle) {
            return Err(format!("Command output does not contain '{needle}'"));
        }
    }

    Ok(format!(
        "Command succeeded with exit code {expected_exit_code}"
    ))
}

fn eval_http(
    url: &str,
    expected_status: u16,
    body_contains: Option<&str>,
) -> Result<String, String> {
    tokio_runtime()?.block_on(http_check(url, expected_status, body_contains))
}

async fn http_check(
    url: &str,
    expected_status: u16,
    body_contains: Option<&str>,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to make HTTP request: {e}"))?;

    let status = response.status().as_u16();
    if status != expected_status {
        return Err(format!("HTTP status {status} (expected {expected_status})"));
    }

    if let Some(needle) = body_contains {
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read HTTP body: {e}"))?;
        let body = String::from_utf8_lossy(&body);
        if !body.contains(needle) {
            return Err(format!("HTTP body does not contain '{needle}'"));
        }
    }

    Ok(format!("HTTP {url} returned status {status}"))
}
