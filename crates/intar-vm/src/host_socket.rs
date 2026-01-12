use crate::VmError;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

#[cfg(unix)]
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

#[cfg(unix)]
use tokio::net::UnixStream;

pub trait HostIo: AsyncRead + AsyncWrite {}

impl<T: AsyncRead + AsyncWrite + ?Sized> HostIo for T {}

pub type HostStream = Box<dyn HostIo + Unpin + Send>;

#[derive(Clone, Debug)]
pub enum HostSocket {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(SocketAddr),
}

impl HostSocket {
    #[cfg(unix)]
    #[must_use]
    pub fn unix(path: PathBuf) -> Self {
        Self::Unix(path)
    }

    #[must_use]
    pub fn tcp(port: u16) -> Self {
        Self::Tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    #[must_use]
    pub fn chardev_arg(&self, id: &str) -> String {
        match self {
            #[cfg(unix)]
            HostSocket::Unix(path) => {
                format!("socket,id={id},path={},server=on,wait=off", path.display())
            }
            HostSocket::Tcp(addr) => format!(
                "socket,id={id},host={},port={},server=on,wait=off",
                addr.ip(),
                addr.port()
            ),
        }
    }

    #[must_use]
    pub fn qmp_arg(&self) -> String {
        match self {
            #[cfg(unix)]
            HostSocket::Unix(path) => format!("unix:{},server,nowait", path.display()),
            HostSocket::Tcp(addr) => format!("tcp:{addr},server,nowait"),
        }
    }

    #[must_use]
    pub fn cleanup_path(&self) -> Option<&Path> {
        match self {
            #[cfg(unix)]
            HostSocket::Unix(path) => Some(path.as_path()),
            HostSocket::Tcp(_) => None,
        }
    }
}

/// Connect to a host socket (Unix on Unix, TCP on Windows).
///
/// # Errors
/// Returns `VmError::Serial` if the socket cannot be opened.
pub async fn connect_host_socket(socket: &HostSocket) -> Result<HostStream, VmError> {
    match socket {
        #[cfg(unix)]
        HostSocket::Unix(path) => UnixStream::connect(path)
            .await
            .map(|stream| Box::new(stream) as HostStream)
            .map_err(|e| VmError::Serial(format!("Failed to connect to socket: {e}"))),
        HostSocket::Tcp(addr) => TcpStream::connect(addr)
            .await
            .map(|stream| Box::new(stream) as HostStream)
            .map_err(|e| VmError::Serial(format!("Failed to connect to socket: {e}"))),
    }
}
