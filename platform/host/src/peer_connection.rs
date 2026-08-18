use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};
use tungstenite::{client as ws_client, Message, WebSocket};

pub struct PeerConnection {
    peer_id: String,
    udp: UdpSocket,
    ws: WebSocket<TcpStream>,
    peers: HashMap<String, SocketAddr>,
}

impl PeerConnection {
    pub fn new(peer_id: impl Into<String>, signal_addr: &str, udp_bind: &str) -> Result<Self> {
        let peer_id = peer_id.into();

        let udp = UdpSocket::bind(udp_bind)?;
        udp.set_nonblocking(true)?;
        let local_addr = udp.local_addr()?;

        let tcp = TcpStream::connect(signal_addr).with_context(|| {
            format!("signaling server at {signal_addr} not reachable (is it running?)")
        })?;
        tcp.set_read_timeout(Some(Duration::from_millis(200)))?;
        let url = format!("ws://{signal_addr}/");
        let (ws, _) = ws_client(url, tcp).context("signaling handshake failed")?;

        let mut pc = Self {
            peer_id,
            udp,
            ws,
            peers: HashMap::new(),
        };
        pc.send_signal(&json!({
            "type": "register",
            "peer_id": pc.peer_id,
            "address": local_addr.to_string(),
        }))?;
        Ok(pc)
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.udp.local_addr()?)
    }

    pub fn peer_addr(&self, peer_id: &str) -> Option<&SocketAddr> {
        self.peers.get(peer_id)
    }

    /// Asks the signaling server to connect us to `target` and waits for the
    /// target's address, which the server relays to us.
    pub fn request_connection(&mut self, target: &str) -> Result<SocketAddr> {
        self.send_signal(&json!({
            "type": "request_connection",
            "from_peer": self.peer_id,
            "to_peer": target,
        }))?;
        let addr = self.wait_for_connection_info(target, Duration::from_secs(5))?;
        Ok(addr)
    }

    /// Waits until the signaling server relays the address of `target`.
    pub fn wait_for_connection_info(&mut self, target: &str, timeout: Duration) -> Result<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            let v = self.read_signal()?;
            match v["type"].as_str() {
                Some("connection_info") if v["peer_id"].as_str() == Some(target) => {
                    let addr = v["address"]
                        .as_str()
                        .ok_or_else(|| anyhow!("missing address"))?
                        .parse()?;
                    self.peers.insert(target.to_string(), addr);
                    return Ok(addr);
                }
                Some("error") => bail!("signaling error: {}", v["message"]),
                _ => {}
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for connection info from '{target}'");
            }
        }
    }

    pub fn send_udp(&self, addr: SocketAddr, data: &[u8]) -> Result<usize> {
        Ok(self.udp.send_to(data, addr)?)
    }

    pub fn recv_udp(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.udp.recv_from(buf) {
                Ok(r) => return Ok(r),
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    if Instant::now() >= deadline {
                        bail!("timed out waiting for UDP message");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Non-blocking receive; returns None if no datagram is pending.
    pub fn try_recv_udp(&self, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
        match self.udp.recv_from(buf) {
            Ok(r) => Some(r),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut =>
            {
                None
            }
            Err(_) => None,
        }
    }

    /// Drains all currently pending UDP datagrams from the socket.
    pub fn poll_incoming(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        while let Ok((len, _)) = self.udp.recv_from(&mut buf) {
            out.push(buf[..len].to_vec());
        }
        out
    }

    /// Drains all pending UDP datagrams, preserving each sender's address.
    pub fn poll_incoming_from(&self) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        while let Ok((len, addr)) = self.udp.recv_from(&mut buf) {
            out.push((addr, buf[..len].to_vec()));
        }
        out
    }

    /// Broadcasts `data` to every currently known peer.
    pub fn send_to_all(&self, data: &[u8]) -> Result<()> {
        for addr in self.peers.values() {
            self.udp.send_to(data, addr)?;
        }
        Ok(())
    }

    /// Reverse-looks up a known peer by its socket address.
    pub fn peer_id_for_addr(&self, addr: &SocketAddr) -> Option<&String> {
        self.peers.iter().find(|(_, a)| *a == addr).map(|(id, _)| id)
    }

    fn send_signal(&mut self, v: &Value) -> Result<()> {
        self.ws.send(Message::text(v.to_string()))?;
        Ok(())
    }

    fn read_signal(&mut self) -> Result<Value> {
        loop {
            match self.ws.read() {
                Ok(Message::Text(t)) => return Ok(serde_json::from_str(t.as_str())?),
                Ok(Message::Close(_)) => bail!("signaling server closed connection"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}
