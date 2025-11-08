use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::ErrorKind;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub(super) struct DatagramStream {
    local_socket: UdpSocket,
    peer_address: SocketAddr,
    display: String,
}

impl DatagramStream {
    pub(super) fn new(local_socket: UdpSocket, peer_address: SocketAddr) -> Self {
        let local_address = local_socket.local_addr().unwrap();
        let local_ip = local_address.ip().to_string();
        let local_port = local_address.port().to_string();
        let remote_ip = peer_address.ip().to_string();
        let remote_port = peer_address.port().to_string();
        let display = format!("{local_ip}:{local_port} <=> {remote_ip}:{remote_port}");
        Self {
            local_socket,
            peer_address,
            display,
        }
    }

    pub(super) fn remote_port(&self) -> u16 {
        self.peer_address.port()
    }

    pub(super) async fn send(&self, buffer: &[u8]) -> std::io::Result<()> {
        match self.local_socket.send_to(buffer, self.peer_address).await {
            Ok(sent) => {
                if sent != buffer.len() {
                    Err(ErrorKind::ConnectionReset.into())
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn recv(&self, buffer: &mut [u8], min_size: usize) -> std::io::Result<usize> {
        loop {
            match self.local_socket.recv_from(buffer).await {
                Ok((recv_size, remote_address)) => {
                    if remote_address != self.peer_address {
                        eprintln!(
                            "{self}: Ignore datagram {recv_size} long from alien {remote_address}"
                        );
                    } else if recv_size < min_size {
                        eprintln!("{self}: Ignore runt datagram {recv_size} long");
                    } else {
                        return Ok(recv_size);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Debug for DatagramStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.display)
    }
}

impl Display for DatagramStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.display)
    }
}
