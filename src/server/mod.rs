use crate::messages::ReadRequest;
use crate::peer_handler::PeerHandler;
use std::collections::HashMap;
use std::fmt::Display;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic;
use std::time::Duration;

#[cfg(test)]
mod tests;

const BUFFER_SIZE: usize = u16::MAX as _;

pub(super) struct TFTPServer {
    socket: UdpSocket,
    root_dir: PathBuf,
    peer_handlers: HashMap<IpAddr, PeerHandler>,
    max_idle_time: Duration,
    buffer: [u8; BUFFER_SIZE],
    display: String,
}

impl TFTPServer {
    pub(super) fn new(socket: UdpSocket, root_dir: PathBuf, idle_timeout: u64) -> Self {
        let max_idle_time = Duration::from_secs(idle_timeout);
        let local_addr = socket
            .local_addr()
            .unwrap_or_else(|err| panic!("Failed to get {socket:?} address: {err}"));
        let display = format!("<TFTP on {}:{}>", local_addr.ip(), local_addr.port());
        Self {
            socket,
            root_dir,
            peer_handlers: HashMap::new(),
            max_idle_time,
            buffer: [0; BUFFER_SIZE],
            display,
        }
    }
    pub(super) fn serve_until_shutdown(
        &mut self,
        shutdown_requested: &atomic::AtomicBool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        eprintln!("{self}: Listening");
        self.socket.set_read_timeout(Some(Duration::new(1, 0)))?;
        loop {
            match self.socket.recv_from(&mut self.buffer) {
                Ok((read_bytes, remote)) => self.handle_request(read_bytes, remote),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if shutdown_requested.load(atomic::Ordering::Relaxed) {
                        break Ok(());
                    } else {
                        self.peer_handlers
                            .retain(|_ip_addr, handler| !handler.is_finished());
                    }
                }
                Err(error) => {
                    break Err(Box::new(error));
                }
            }
        }
    }

    fn handle_request(&mut self, size: usize, remote: SocketAddr) {
        match ReadRequest::parse(&self.buffer[..size]) {
            Ok(rrq) => {
                eprintln!("Received {rrq} from {remote}");
                let local_ip = self.socket.local_addr().unwrap().ip();
                let remote_ip = remote.ip();
                let handler = self.peer_handlers.entry(remote_ip).or_insert_with(|| {
                    PeerHandler::new(
                        remote_ip,
                        local_ip,
                        self.root_dir.clone(),
                        self.max_idle_time,
                    )
                });
                if !handler.feed(remote.port(), rrq) {
                    eprintln!("{handler}: Failed to feed. Shutting down ...");
                    if let Some(handler) = self.peer_handlers.remove(&remote_ip) {
                        handler.shutdown();
                    }
                }
            }
            Err(tftp_error) => {
                eprintln!("{remote}: RRQ parsing error: {tftp_error}");
                if let Ok(size) = tftp_error.serialize(&mut self.buffer) {
                    if self.socket.send_to(&self.buffer[..size], remote).is_err() {
                        eprintln!("{remote}: Error sending {tftp_error:?}");
                    }
                }
            }
        }
    }
}

impl Display for TFTPServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

impl Drop for TFTPServer {
    fn drop(&mut self) {
        for (_addr, handler) in self.peer_handlers.drain() {
            handler.shutdown()
        }
    }
}
