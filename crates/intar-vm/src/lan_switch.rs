use crate::VmError;
use socket2::SockRef;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

pub struct LanSwitch {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LanSwitch {
    /// Spawn a lightweight L2 switch that forwards raw Ethernet frames between peers.
    ///
    /// Each peer is expected to be a localhost UDP endpoint used by QEMU's `-netdev dgram`.
    ///
    /// # Errors
    /// Returns `VmError` if the hub socket cannot be bound/configured or the switch thread cannot
    /// be started.
    pub fn spawn(hub_port: u16, peers: Vec<SocketAddr>) -> Result<Self, VmError> {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, hub_port))
            .map_err(|e| VmError::Qemu(format!("Failed to bind LAN hub UDP socket: {e}")))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|e| VmError::Qemu(format!("Failed to configure LAN hub socket: {e}")))?;

        let sock_ref = SockRef::from(&socket);
        if let Err(e) = sock_ref.set_recv_buffer_size(4 * 1024 * 1024) {
            warn!("Failed to increase LAN hub recv buffer: {e}");
        }
        if let Err(e) = sock_ref.set_send_buffer_size(4 * 1024 * 1024) {
            warn!("Failed to increase LAN hub send buffer: {e}");
        }

        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("intar-lan-switch".into())
            .spawn(move || run_switch(&socket, &peers, &stop_rx))
            .map_err(|e| VmError::Qemu(format!("Failed to start LAN switch thread: {e}")))?;

        Ok(Self {
            stop_tx: Some(stop_tx),
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LanSwitch {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_switch(socket: &UdpSocket, peers: &[SocketAddr], stop_rx: &mpsc::Receiver<()>) {
    let mut mac_table: HashMap<[u8; 6], SocketAddr> = HashMap::new();
    let mut buf = vec![0u8; 2048];

    info!(
        hub = %socket.local_addr().map_or_else(|_| "<unknown>".into(), |a| a.to_string()),
        peers = peers.len(),
        "LAN switch started"
    );

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if n < 14 {
                    continue;
                }
                let frame = &buf[..n];

                let dst: [u8; 6] = match frame[0..6].try_into() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let src: [u8; 6] = match frame[6..12].try_into() {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                mac_table.insert(src, from);

                let is_broadcast = dst.iter().all(|b| *b == 0xff);
                let is_multicast = (dst[0] & 0x01) == 0x01;

                if is_broadcast || is_multicast {
                    for peer in peers {
                        if *peer != from {
                            let _ = socket.send_to(frame, peer);
                        }
                    }
                    continue;
                }

                if let Some(target) = mac_table.get(&dst) {
                    if *target != from {
                        let _ = socket.send_to(frame, target);
                    }
                } else {
                    for peer in peers {
                        if *peer != from {
                            let _ = socket.send_to(frame, peer);
                        }
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => {
                warn!("LAN switch recv error: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    info!("LAN switch stopped");
}
