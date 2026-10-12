//! Govee Wi-Fi lights through the LAN API ("LAN Control" has to be on for each light in the Govee app).
//!
//! Two things learned on real hardware: lights only answer `devStatus` to a computer that scanned
//! for them recently, and control commands sent back to back get dropped, so they go out ~200 ms
//! apart. Streamed frames don't need that spacing.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lightsync_core::govee::{self as gp, DeviceState, LanDevice, LanReply, Rgb};
use lightsync_core::settings::{LanMode, OnExit};
use socket2::{Domain, Protocol, SockRef, Socket, Type};

use super::{Light, average};
use crate::log::log;

const COMMAND_GAP: Duration = Duration::from_millis(200);

/// The one UDP socket on port 4002, where every light replies. Shared by all LAN lights.
pub struct LanBus {
    socket: UdpSocket,
    subscribers: Mutex<Vec<Sender<(Ipv4Addr, LanReply)>>>,
    known: Mutex<HashMap<String, LanDevice>>,
    last_scan: Mutex<Option<Instant>>,
}

impl LanBus {
    pub fn open() -> Result<Arc<Self>> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // Govee's own desktop app may be holding this port too
        socket.set_reuse_address(true)?;
        socket
            .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, gp::LISTEN_PORT)).into())
            .context("couldn't listen on UDP port 4002 for the lights")?;
        socket.set_multicast_ttl_v4(2)?;
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let socket: UdpSocket = socket.into();
        let reader = socket.try_clone()?;
        let bus = Arc::new(Self {
            socket,
            subscribers: Mutex::new(Vec::new()),
            known: Mutex::new(HashMap::new()),
            last_scan: Mutex::new(None),
        });
        let weak = Arc::downgrade(&bus);
        std::thread::Builder::new().name("lan replies".into()).spawn(move || listen(reader, weak))?;
        Ok(bus)
    }

    pub fn send(&self, payload: &[u8], ip: Ipv4Addr) {
        let _ = self.socket.send_to(payload, SocketAddrV4::new(ip, gp::CONTROL_PORT));
    }

    fn subscribe(&self) -> Receiver<(Ipv4Addr, LanReply)> {
        let (tx, rx) = channel();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Asks every light on the network to say where it is.
    pub fn scan(&self, duration: Duration) -> Vec<LanDevice> {
        let replies = self.subscribe();
        let interfaces = local_ipv4();
        let start = Instant::now();
        let mut found: Vec<LanDevice> = Vec::new();
        let mut next_probe = start;
        while start.elapsed() < duration {
            if Instant::now() >= next_probe {
                // Out of every interface: Windows otherwise picks one, often a VPN or Hyper-V adapter
                let sock = SockRef::from(&self.socket);
                for interface in &interfaces {
                    let _ = sock.set_multicast_if_v4(interface);
                    let _ = self.socket.send_to(&gp::lan_scan(), SocketAddrV4::new(gp::MULTICAST_ADDR, gp::SCAN_PORT));
                }
                next_probe += duration / 3;
            }
            if let Ok((_, LanReply::Scan(device))) = replies.recv_timeout(Duration::from_millis(100))
                && !found.iter().any(|d| d.device_id == device.device_id)
            {
                found.push(device);
            }
        }
        let mut known = self.known.lock().unwrap();
        for device in &found {
            known.insert(device.device_id.clone(), device.clone());
        }
        *self.last_scan.lock().unwrap() = Some(Instant::now());
        found
    }

    /// Scans unless one ran in the last `max_age`.
    pub fn ensure_scanned(&self, max_age: Duration) {
        let fresh = self.last_scan.lock().unwrap().is_some_and(|t| t.elapsed() < max_age);
        if !fresh {
            self.scan(Duration::from_millis(1500));
        }
    }

    pub fn resolve(&self, device_id: &str) -> Option<Ipv4Addr> {
        self.known.lock().unwrap().get(device_id).map(|d| d.ip)
    }

    pub fn status(&self, ip: Ipv4Addr, timeout: Duration) -> Option<DeviceState> {
        let replies = self.subscribe();
        self.send(&gp::lan_status(), ip);
        let start = Instant::now();
        while let Some(left) = timeout.checked_sub(start.elapsed()) {
            match replies.recv_timeout(left) {
                Ok((from, LanReply::Status(state))) if from == ip => return Some(state),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        None
    }
}

fn listen(socket: UdpSocket, bus: Weak<LanBus>) {
    let mut buffer = [0u8; 2048];
    loop {
        let received = socket.recv_from(&mut buffer);
        let Some(bus) = bus.upgrade() else { return };
        if let Ok((n, SocketAddr::V4(from))) = received
            && let Some(reply) = gp::parse_reply(&buffer[..n])
        {
            bus.subscribers.lock().unwrap().retain(|tx| tx.send((*from.ip(), reply.clone())).is_ok());
        }
    }
}

fn local_ipv4() -> Vec<Ipv4Addr> {
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into());
    let mut addresses: Vec<Ipv4Addr> = (host.as_str(), 0)
        .to_socket_addrs()
        .map(|it| {
            it.filter_map(|a| match a {
                SocketAddr::V4(v4) if !v4.ip().is_loopback() => Some(*v4.ip()),
                _ => None,
            })
            .collect()
        })
        .unwrap_or_default();
    addresses.dedup();
    if addresses.is_empty() {
        addresses.push(Ipv4Addr::UNSPECIFIED);
    }
    addresses
}

pub struct LanLight {
    bus: Arc<LanBus>,
    name: String,
    device_id: String,
    ip: Option<Ipv4Addr>,
    mode: LanMode,
    gradient: bool,
    on_exit: OnExit,
    saved: Option<DeviceState>,
    last: Option<Vec<u8>>,
}

impl LanLight {
    pub fn new(
        bus: Arc<LanBus>,
        name: &str,
        device_id: &str,
        ip: &str,
        mode: LanMode,
        gradient: bool,
        on_exit: OnExit,
    ) -> Self {
        Self {
            bus,
            name: name.into(),
            device_id: device_id.into(),
            ip: ip.parse().ok(),
            mode,
            gradient,
            on_exit,
            saved: None,
            last: None,
        }
    }

    fn command(&self, payload: &[u8]) {
        if let Some(ip) = self.ip {
            self.bus.send(payload, ip);
            std::thread::sleep(COMMAND_GAP);
        }
    }
}

impl Light for LanLight {
    fn connect(&mut self) -> Result<()> {
        self.bus.ensure_scanned(Duration::from_secs(300));
        let mut ip = self.bus.resolve(&self.device_id);
        if ip.is_none() {
            // It may have just joined, or its IP changed: look again before falling back
            self.bus.scan(Duration::from_secs(3));
            ip = self.bus.resolve(&self.device_id);
        }
        self.ip = ip.or(self.ip);
        let ip = self.ip.context("not found on the network. Is it on, with LAN Control enabled in the Govee app?")?;
        if self.on_exit == OnExit::Restore && self.saved.is_none() {
            self.saved = self.bus.status(ip, Duration::from_millis(1500));
            if self.saved.is_none() {
                log!("{}: no status reply from {ip}, so its colour can't be restored later", self.name);
            }
        }
        self.command(&gp::lan_turn(true));
        if self.mode == LanMode::Razer {
            self.command(&gp::lan_razer(&gp::razer_enable(true)));
        }
        self.last = None;
        Ok(())
    }

    fn send(&mut self, colors: &[Rgb]) -> Result<()> {
        let ip = self.ip.context("not connected")?;
        let payload = match self.mode {
            LanMode::Razer => gp::lan_razer(&gp::razer_frame(colors, self.gradient)),
            LanMode::Color => gp::lan_color(average(colors), 0),
        };
        self.bus.send(&payload, ip);
        self.last = Some(payload);
        Ok(())
    }

    fn keepalive(&mut self) -> Result<()> {
        // Lights leave streaming mode after a while without data, or when the Govee app touches them
        let ip = self.ip.context("not connected")?;
        if self.mode == LanMode::Razer {
            self.command(&gp::lan_razer(&gp::razer_enable(true)));
        }
        if let Some(last) = &self.last {
            self.bus.send(last, ip);
        }
        Ok(())
    }

    fn sleep(&mut self) {
        if self.mode == LanMode::Razer {
            self.command(&gp::lan_razer(&gp::razer_enable(false)));
        }
        self.command(&gp::lan_turn(false));
        self.last = None;
    }

    fn close(&mut self, restore: bool) {
        if self.mode == LanMode::Razer {
            self.command(&gp::lan_razer(&gp::razer_enable(false)));
        }
        if !restore {
            return;
        }
        match (self.on_exit, self.saved) {
            (OnExit::Off, _) => self.command(&gp::lan_turn(false)),
            (OnExit::Restore, Some(state)) => {
                self.command(&gp::lan_color(state.color, state.kelvin));
                self.command(&gp::lan_brightness(state.brightness));
                self.command(&gp::lan_turn(state.on));
            }
            _ => {}
        }
    }
}
