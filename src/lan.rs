//! LAN backend: discovers and controls bulbs directly over UDP using the
//! LIFX LAN protocol (port 56700), no cloud involved.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use lifx_core::{BuildOptions, Message, RawMessage, Service, HSBK};

use crate::model::{Backend, BulbState, DeviceKind, Event, Hsbk, LanCommand, Subnet};

/// Arbitrary non-zero client id; bulbs echo it back so we can tell our own
/// traffic apart, and a non-zero source makes bulbs reply via unicast.
const SOURCE: u32 = 0x1F1F_CA4E;

const DISCOVER_INTERVAL: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const OFFLINE_AFTER: Duration = Duration::from_secs(20);
/// After we push a change, ignore poll replies briefly so a stale in-flight
/// state report doesn't yank the UI controls backwards.
const SUPPRESS_AFTER_COMMAND: Duration = Duration::from_millis(1500);

struct Device {
    addr: SocketAddr,
    label: Option<String>,
    group: Option<String>,
    color: Option<Hsbk>,
    powered: bool,
    connected: bool,
    /// (vendor id, product id) from StateVersion.
    version: Option<(u32, u32)>,
    /// (major, minor) from StateHostFirmware.
    firmware: Option<(u16, u16)>,
    last_seen: Instant,
    suppress_until: Instant,
}

pub fn serial_hex(target: u64) -> String {
    let b = target.to_le_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    )
}

pub fn spawn(events: async_channel::Sender<Event>, commands: Receiver<LanCommand>) {
    std::thread::Builder::new()
        .name("lifx-lan".into())
        .spawn(move || {
            if let Err(e) = run(events, commands) {
                eprintln!("LIFX LAN thread exited with error: {e}");
            }
        })
        .expect("failed to spawn LAN thread");
}

fn run(
    events: async_channel::Sender<Event>,
    commands: Receiver<LanCommand>,
) -> std::io::Result<()> {
    // Prefer the well-known LIFX port: broadcast replies aren't part of a
    // conntrack flow, so on firewalled systems they only get through if the
    // local port is predictable/allowed. Fall back to an ephemeral port if
    // another LIFX client already owns 56700.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 56700))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(50)))?;

    let mut devices: HashMap<u64, Device> = HashMap::new();
    let mut subnets: Vec<Subnet> = Vec::new();
    let mut buf = [0u8; 1024];
    let mut last_discover: Option<Instant> = None;
    let mut last_poll = Instant::now();

    loop {
        loop {
            match commands.try_recv() {
                Ok(cmd) => handle_command(&socket, &mut devices, &mut subnets, cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        if last_discover.is_none_or(|t| t.elapsed() >= DISCOVER_INTERVAL) {
            discover(&socket, &subnets);
            last_discover = Some(Instant::now());
        }

        if last_poll.elapsed() >= POLL_INTERVAL {
            last_poll = Instant::now();
            for (target, dev) in devices.iter_mut() {
                send(&socket, Some(*target), dev.addr, Message::LightGet, true);
                if dev.group.is_none() {
                    send(&socket, Some(*target), dev.addr, Message::GetGroup, true);
                }
                if dev.version.is_none() {
                    send(&socket, Some(*target), dev.addr, Message::GetVersion, true);
                }
                if dev.firmware.is_none() {
                    send(&socket, Some(*target), dev.addr, Message::GetHostFirmware, true);
                }
                if dev.connected && dev.last_seen.elapsed() > OFFLINE_AFTER {
                    dev.connected = false;
                    emit(&events, *target, dev);
                }
            }
        }

        match socket.recv_from(&mut buf) {
            Ok((n, src)) => handle_packet(&buf[..n], src, &socket, &mut devices, &events),
            Err(e)
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
}

fn discover(socket: &UdpSocket, subnets: &[Subnet]) {
    let addr = SocketAddr::from((Ipv4Addr::BROADCAST, 56700));
    send(socket, None, addr, Message::GetService, true);

    // Broadcast doesn't cross subnets, so probe configured remote subnets
    // (e.g. an isolated IoT VLAN) directly: a directed broadcast in case the
    // router forwards it, plus a unicast sweep, which always routes.
    for subnet in subnets {
        let addr = SocketAddr::from((subnet.broadcast(), 56700));
        send(socket, None, addr, Message::GetService, true);
        for host in subnet.hosts() {
            let addr = SocketAddr::from((host, 56700));
            send(socket, None, addr, Message::GetService, true);
        }
    }
}

fn handle_command(
    socket: &UdpSocket,
    devices: &mut HashMap<u64, Device>,
    subnets: &mut Vec<Subnet>,
    cmd: LanCommand,
) {
    match cmd {
        LanCommand::Discover => discover(socket, subnets),
        LanCommand::SetSubnets(list) => {
            *subnets = list;
            discover(socket, subnets);
        }
        LanCommand::SetPower { target, on } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.powered = on;
                dev.suppress_until = Instant::now() + SUPPRESS_AFTER_COMMAND;
                let msg = Message::LightSetPower {
                    level: if on { 65535 } else { 0 },
                    duration: 300,
                };
                send(socket, Some(target), dev.addr, msg, false);
            }
        }
        LanCommand::SetColor {
            target,
            color,
            duration_ms,
        } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.color = Some(color);
                dev.suppress_until = Instant::now() + SUPPRESS_AFTER_COMMAND;
                let msg = Message::LightSetColor {
                    reserved: 0,
                    color: to_lifx(color),
                    duration: duration_ms,
                };
                send(socket, Some(target), dev.addr, msg, false);
            }
        }
    }
}

fn handle_packet(
    data: &[u8],
    src: SocketAddr,
    socket: &UdpSocket,
    devices: &mut HashMap<u64, Device>,
    events: &async_channel::Sender<Event>,
) {
    let Ok(raw) = RawMessage::unpack(data) else {
        return;
    };
    // Ignore traffic that isn't a reply to us (e.g. other LIFX clients).
    if raw.frame.source != SOURCE {
        return;
    }
    let target = raw.frame_addr.target;
    let Ok(msg) = Message::from_raw(&raw) else {
        return;
    };

    match msg {
        Message::StateService { service, port } => {
            if service != Service::UDP || port == 0 {
                return;
            }
            let addr = SocketAddr::new(src.ip(), port as u16);
            let dev = devices.entry(target).or_insert_with(|| Device {
                addr,
                label: None,
                group: None,
                color: None,
                powered: false,
                connected: false,
                version: None,
                firmware: None,
                last_seen: Instant::now(),
                suppress_until: Instant::now(),
            });
            dev.addr = addr;
            dev.last_seen = Instant::now();
            // Fetch full state right away so new bulbs appear quickly.
            send(socket, Some(target), addr, Message::LightGet, true);
            send(socket, Some(target), addr, Message::GetGroup, true);
        }
        Message::LightState {
            color,
            power,
            label,
            ..
        } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.last_seen = Instant::now();
                let was_connected = dev.connected;
                dev.connected = true;
                let suppressed = Instant::now() < dev.suppress_until;
                if !suppressed {
                    dev.color = Some(from_lifx(color));
                    dev.powered = power > 0;
                }
                dev.label = Some(label.to_string());
                if !suppressed || !was_connected {
                    emit(events, target, dev);
                }
            }
        }
        Message::LightStatePower { level } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.last_seen = Instant::now();
                if Instant::now() >= dev.suppress_until {
                    dev.powered = level > 0;
                    emit(events, target, dev);
                }
            }
        }
        Message::StateGroup { label, .. } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.last_seen = Instant::now();
                let name = label.to_string();
                dev.group = if name.is_empty() { None } else { Some(name) };
                emit(events, target, dev);
            }
        }
        Message::StateVersion {
            vendor, product, ..
        } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.last_seen = Instant::now();
                dev.version = Some((vendor, product));
                emit(events, target, dev);
            }
        }
        Message::StateHostFirmware {
            version_major,
            version_minor,
            ..
        } => {
            if let Some(dev) = devices.get_mut(&target) {
                dev.last_seen = Instant::now();
                dev.firmware = Some((version_major, version_minor));
                emit(events, target, dev);
            }
        }
        _ => {}
    }
}

/// Vendor/device facts shown in the Details dialog. A LIFX serial doubles
/// as the bulb's MAC address.
fn details(target: u64, dev: &Device) -> Vec<(String, String)> {
    let b = target.to_le_bytes();
    let mut out = vec![
        ("Serial".to_string(), serial_hex(target)),
        (
            "MAC address".to_string(),
            format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            ),
        ),
        ("IP address".to_string(), dev.addr.to_string()),
    ];
    if let Some((vendor, product)) = dev.version {
        let name = lifx_core::get_product_info(vendor, product)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| format!("Vendor {vendor} · Product {product}"));
        out.push(("Product".to_string(), name));
    }
    if let Some((major, minor)) = dev.firmware {
        out.push(("Firmware".to_string(), format!("{major}.{minor}")));
    }
    out
}

fn emit(events: &async_channel::Sender<Event>, target: u64, dev: &Device) {
    let (Some(color), Some(label)) = (dev.color, dev.label.as_ref()) else {
        return;
    };
    let _ = events.send_blocking(Event::Upsert(BulbState {
        id: serial_hex(target),
        backend: Backend::Lan,
        kind: DeviceKind::Bulb,
        label: label.clone(),
        group: dev.group.clone(),
        powered: dev.powered,
        color,
        connected: dev.connected,
        lan_target: Some(target),
        details: details(target, dev),
    }));
}

fn send(socket: &UdpSocket, target: Option<u64>, addr: SocketAddr, msg: Message, res: bool) {
    let opts = BuildOptions {
        target,
        ack_required: false,
        res_required: res,
        sequence: 0,
        source: SOURCE,
    };
    if let Ok(raw) = RawMessage::build(&opts, msg) {
        if let Ok(bytes) = raw.pack() {
            let _ = socket.send_to(&bytes, addr);
        }
    }
}

fn to_lifx(c: Hsbk) -> HSBK {
    HSBK {
        hue: c.hue,
        saturation: c.saturation,
        brightness: c.brightness,
        kelvin: c.kelvin,
    }
}

fn from_lifx(c: HSBK) -> Hsbk {
    Hsbk {
        hue: c.hue,
        saturation: c.saturation,
        brightness: c.brightness,
        kelvin: c.kelvin,
    }
}
