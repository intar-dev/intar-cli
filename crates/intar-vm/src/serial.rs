use crate::{HostSocket, VmError, connect_host_socket};
use intar_probes::{ProbeResult, ProbeSpec, Request, Response};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{Duration, timeout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedResponse {
    Pong,
    ProbeResult,
    AllResults,
}

impl ExpectedResponse {
    fn matches(self, response: &Response) -> bool {
        match self {
            ExpectedResponse::Pong => matches!(response, Response::Pong { .. }),
            ExpectedResponse::ProbeResult => matches!(response, Response::ProbeResult { .. }),
            ExpectedResponse::AllResults => matches!(response, Response::AllResults { .. }),
        }
    }
}

pub struct AgentConnection {
    stream: BufReader<crate::HostStream>,
}

impl AgentConnection {
    /// Open a host socket connection to the guest agent.
    ///
    /// # Errors
    /// Returns `VmError::Serial` when the socket cannot be opened.
    pub async fn connect(socket: &HostSocket) -> Result<Self, VmError> {
        let stream = connect_host_socket(socket).await?;

        Ok(Self {
            stream: BufReader::new(stream),
        })
    }

    /// Send a ping request to the agent.
    ///
    /// # Errors
    /// Returns `VmError` when the agent does not respond or replies with an error.
    pub async fn ping(&mut self) -> Result<u64, VmError> {
        let response = self
            .send_request_expect(&Request::Ping, ExpectedResponse::Pong)
            .await?;

        let Response::Pong { uptime_secs } = response else {
            return Err(VmError::Serial("Unexpected response to ping".into()));
        };

        Ok(uptime_secs)
    }

    /// Send a single probe request to the agent.
    ///
    /// # Errors
    /// Returns `VmError` if the agent returns an error or the request fails.
    pub async fn check_probe(
        &mut self,
        id: &str,
        spec: &ProbeSpec,
    ) -> Result<ProbeResult, VmError> {
        let request = Request::CheckProbe {
            id: id.to_string(),
            spec: spec.clone(),
        };

        let response = self
            .send_request_expect(&request, ExpectedResponse::ProbeResult)
            .await?;

        let Response::ProbeResult {
            id,
            passed,
            message,
        } = response
        else {
            return Err(VmError::Serial("Unexpected response to check_probe".into()));
        };

        Ok(ProbeResult {
            id,
            passed,
            message,
        })
    }

    /// Send multiple probes in one request to the agent.
    ///
    /// # Errors
    /// Returns `VmError` if the agent returns an error or the request fails.
    pub async fn check_all(
        &mut self,
        probes: Vec<(String, ProbeSpec)>,
    ) -> Result<Vec<ProbeResult>, VmError> {
        let request = Request::CheckAll { probes };
        let response = self
            .send_request_expect(&request, ExpectedResponse::AllResults)
            .await?;

        let Response::AllResults { results } = response else {
            return Err(VmError::Serial("Unexpected response to check_all".into()));
        };

        Ok(results)
    }

    /// Send a request over the serial socket and wait for the expected response.
    ///
    /// # Errors
    /// Returns `VmError` when the request cannot be written, parsed, or times out.
    async fn send_request_expect(
        &mut self,
        request: &Request,
        expected: ExpectedResponse,
    ) -> Result<Response, VmError> {
        let request_json = serde_json::to_string(request)?;

        self.stream
            .get_mut()
            .write_all(format!("{request_json}\n").as_bytes())
            .await
            .map_err(|e| VmError::Serial(format!("Failed to send request: {e}")))?;

        self.stream
            .get_mut()
            .flush()
            .await
            .map_err(|e| VmError::Serial(format!("Failed to flush: {e}")))?;

        let deadline = Instant::now() + Duration::from_secs(30);

        loop {
            let now = Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return Err(VmError::Timeout("Agent response timeout".into()));
            }

            let mut response_line = String::new();
            let read_result = timeout(remaining, self.stream.read_line(&mut response_line)).await;

            let bytes = match read_result {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(e)) => return Err(VmError::Serial(format!("Failed to read response: {e}"))),
                Err(_) => return Err(VmError::Timeout("Agent response timeout".into())),
            };

            if bytes == 0 {
                return Err(VmError::Serial(
                    "Unexpected EOF reading agent response".into(),
                ));
            }

            let line = response_line.trim();
            if line.is_empty() {
                continue;
            }

            let response: Response = serde_json::from_str(line)?;

            match response {
                Response::Error { message } => return Err(VmError::Serial(message)),
                other if expected.matches(&other) => return Ok(other),
                _ => {}
            }
        }
    }
}

/// Attempt to connect to the agent with retries.
///
/// # Errors
/// Returns the last `VmError` if all attempts fail.
pub async fn try_connect(
    socket: &HostSocket,
    retries: u32,
    delay_ms: u64,
) -> Result<AgentConnection, VmError> {
    for i in 0..retries {
        match AgentConnection::connect(socket).await {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                if i < retries - 1 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(VmError::Serial("Failed to connect after retries".into()))
}
