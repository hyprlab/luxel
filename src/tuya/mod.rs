//! Tuya/SmartLife backend: controls smart plugs directly over the LAN
//! (TCP port 6668), no cloud involved.
//!
//! Unlike LIFX, Tuya's local protocol encrypts everything with a per-device
//! local key that only the Tuya cloud can reveal, so devices are configured
//! by hand (IP + device id + local key) in Preferences rather than
//! discovered. UDP broadcasts (ports 6666/6667) are still listened to: when
//! a configured device announces itself we pick up its current IP and
//! protocol version automatically.

pub mod cloud;
mod protocol;

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use crate::config::TuyaDevice as TuyaConfig;
use crate::model::{Backend, BulbState, DeviceKind, Event, Hsbk, Subnet, TuyaCommand};
use protocol::{Codec, Version};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Read timeout while waiting for a synchronous reply (handshake / probe).
const REPLY_TIMEOUT: Duration = Duration::from_millis(2000);
const OFFLINE_AFTER: Duration = Duration::from_secs(20);
/// After we push a change, ignore state reports briefly so a stale in-flight
/// report doesn't yank the UI controls backwards.
const SUPPRESS_AFTER_COMMAND: Duration = Duration::from_millis(1500);

/// Version candidates tried in turn when the configured version is "auto".
const AUTO_VERSIONS: [Version; 3] = [Version::V33, Version::V34, Version::V35];

struct Conn {
    stream: TcpStream,
    codec: Codec,
    seqno: u32,
    rbuf: Vec<u8>,
}

impl Conn {
    fn send(&mut self, cmd: u32, payload: &[u8]) -> std::io::Result<()> {
        self.seqno += 1;
        let frame = self.codec.encode(self.seqno, cmd, payload);
        self.stream.write_all(&frame)
    }
}

struct Device {
    cfg: TuyaConfig,
    conn: Option<Conn>,
    /// Protocol version in use; fixed by config, learned by auto-detection,
    /// or reported by a UDP broadcast.
    version: Option<Version>,
    /// Next candidate to try while auto-detecting.
    probe_idx: usize,
    /// The boolean data point that is this device's power switch.
    switch_dp: Option<String>,
    /// Latest data points reported by the device, for the Details dialog.
    last_dps: serde_json::Map<String, serde_json::Value>,
    powered: bool,
    connected: bool,
    last_ok: Instant,
    next_attempt: Instant,
    suppress_until: Instant,
}

impl Device {
    fn new(cfg: TuyaConfig) -> Device {
        let now = Instant::now();
        Device {
            version: Version::parse(cfg.version.trim()),
            cfg,
            conn: None,
            probe_idx: 0,
            switch_dp: None,
            last_dps: serde_json::Map::new(),
            powered: false,
            connected: false,
            last_ok: now,
            next_attempt: now,
            suppress_until: now,
        }
    }

    fn key(&self) -> [u8; 16] {
        key16(&self.cfg.key)
    }

    fn label(&self) -> String {
        let name = self.cfg.name.trim();
        if name.is_empty() {
            format!("Smart Plug ({})", self.cfg.host.trim())
        } else {
            name.to_string()
        }
    }
}

pub fn spawn(events: async_channel::Sender<Event>, commands: Receiver<TuyaCommand>) {
    std::thread::Builder::new()
        .name("tuya-lan".into())
        .spawn(move || run(events, commands))
        .expect("failed to spawn Tuya thread");
}

fn key16(key: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = key.trim().as_bytes();
    let n = bytes.len().min(16);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn emit(events: &async_channel::Sender<Event>, dev: &Device) {
    let mut details = vec![
        ("Device ID".to_string(), dev.cfg.id.trim().to_string()),
        (
            "IP address".to_string(),
            format!("{}:6668", dev.cfg.host.trim()),
        ),
        (
            "Protocol".to_string(),
            dev.version
                .map(|v| format!("Tuya {}", v.as_str()))
                .unwrap_or_else(|| "Tuya (detecting…)".to_string()),
        ),
    ];
    if let Some(dp) = &dev.switch_dp {
        details.push(("Switch data point".to_string(), dp.clone()));
    }
    // Raw data points straight from the device, numerically ordered. A few
    // internal/vendor bookkeeping points add noise, so they're hidden; the
    // switch data point reads as a friendly ON/OFF state line.
    const HIDDEN_DPS: [&str; 4] = ["11", "101", "102", "103"];
    let mut dps: Vec<(&String, &serde_json::Value)> = dev.last_dps.iter().collect();
    dps.sort_by_key(|(k, _)| k.parse::<u32>().unwrap_or(u32::MAX));
    for (k, v) in dps {
        if HIDDEN_DPS.contains(&k.as_str()) {
            continue;
        }
        if Some(k) == dev.switch_dp.as_ref() {
            let value = match v.as_bool() {
                Some(true) => "ON".to_string(),
                Some(false) => "OFF".to_string(),
                None => v.to_string(),
            };
            details.push((format!("State (Data point {k})"), value));
        } else {
            details.push((format!("Data point {k}"), v.to_string()));
        }
    }
    let _ = events.send_blocking(Event::Upsert(BulbState {
        id: format!("tuya:{}", dev.cfg.id.trim()),
        backend: Backend::Tuya,
        kind: DeviceKind::Plug,
        label: dev.label(),
        group: None,
        powered: dev.powered,
        color: Hsbk::default(),
        connected: dev.connected,
        lan_target: None,
        details,
    }));
}

fn run(events: async_channel::Sender<Event>, commands: Receiver<TuyaCommand>) {
    // Passive discovery listeners; may fail (e.g. another Tuya app owns the
    // port), in which case we simply do without.
    let udp: Vec<UdpSocket> = [6666u16, 6667]
        .iter()
        .filter_map(|port| {
            let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, *port)).ok()?;
            s.set_nonblocking(true).ok()?;
            Some(s)
        })
        .collect();

    let mut devices: HashMap<String, Device> = HashMap::new();
    let mut last_poll = Instant::now() - POLL_INTERVAL;

    loop {
        loop {
            match commands.try_recv() {
                Ok(cmd) => handle_command(&events, &mut devices, &mut last_poll, cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if last_poll.elapsed() >= POLL_INTERVAL {
            last_poll = Instant::now();
            for dev in devices.values_mut() {
                poll_device(&events, dev);
            }
        }

        listen_udp(&udp, &mut devices);
        let any_read = read_connections(&events, &mut devices);
        if !any_read {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn handle_command(
    events: &async_channel::Sender<Event>,
    devices: &mut HashMap<String, Device>,
    last_poll: &mut Instant,
    cmd: TuyaCommand,
) {
    match cmd {
        TuyaCommand::Configure(list) => {
            let mut next: HashMap<String, Device> = HashMap::new();
            for cfg in list {
                if !cfg.is_complete() {
                    continue;
                }
                let id = cfg.id.trim().to_string();
                match devices.remove(&id) {
                    // Connection-relevant fields unchanged: keep the live
                    // connection and state, adopt cosmetic changes (name).
                    Some(mut dev)
                        if dev.cfg.host == cfg.host
                            && dev.cfg.id == cfg.id
                            && dev.cfg.key == cfg.key
                            && dev.cfg.version == cfg.version =>
                    {
                        dev.cfg = cfg;
                        next.insert(id, dev);
                    }
                    _ => {
                        next.insert(id, Device::new(cfg));
                    }
                }
            }
            *devices = next;
            for dev in devices.values() {
                emit(events, dev);
            }
            *last_poll = Instant::now() - POLL_INTERVAL;
        }
        TuyaCommand::Refresh => {
            let now = Instant::now();
            for dev in devices.values_mut() {
                dev.next_attempt = now;
            }
            *last_poll = now - POLL_INTERVAL;
        }
        TuyaCommand::SetPower { id, on } => {
            let devid = id.strip_prefix("tuya:").unwrap_or(&id);
            if let Some(dev) = devices.get_mut(devid) {
                dev.powered = on;
                dev.suppress_until = Instant::now() + SUPPRESS_AFTER_COMMAND;
                emit(events, dev);
                if dev.conn.is_none() {
                    connect_device(events, dev);
                }
                let dp = dev.switch_dp.clone().unwrap_or_else(|| "1".to_string());
                if let Some(conn) = dev.conn.as_mut() {
                    let sent = match conn.codec.version {
                        Version::V33 => {
                            let devid = dev.cfg.id.trim();
                            let json = format!(
                                r#"{{"devId":"{devid}","uid":"{devid}","t":"{}","dps":{{"{dp}":{on}}}}}"#,
                                epoch()
                            );
                            conn.send(protocol::CONTROL, json.as_bytes())
                        }
                        _ => {
                            let json = format!(
                                r#"{{"protocol":5,"t":{},"data":{{"dps":{{"{dp}":{on}}}}}}}"#,
                                epoch()
                            );
                            conn.send(protocol::CONTROL_NEW, json.as_bytes())
                        }
                    };
                    if sent.is_err() {
                        drop_connection(events, dev);
                    }
                }
            }
        }
        TuyaCommand::Locate { devices, subnets } => {
            locate(events, devices, subnets);
        }
    }
}

fn poll_device(events: &async_channel::Sender<Event>, dev: &mut Device) {
    if dev.conn.is_none() {
        if Instant::now() >= dev.next_attempt {
            connect_device(events, dev);
        }
        return;
    }
    // Connection open but silent for too long: treat as gone.
    if dev.last_ok.elapsed() > OFFLINE_AFTER {
        drop_connection(events, dev);
        return;
    }
    let conn = dev.conn.as_mut().unwrap();
    let sent = match conn.codec.version {
        Version::V33 => {
            let devid = dev.cfg.id.trim();
            let json = format!(
                r#"{{"gwId":"{devid}","devId":"{devid}","uid":"{devid}","t":"{}"}}"#,
                epoch()
            );
            conn.send(protocol::DP_QUERY, json.as_bytes())
        }
        _ => conn.send(protocol::DP_QUERY_NEW, b"{}"),
    };
    if sent.is_err() {
        drop_connection(events, dev);
    }
}

fn drop_connection(events: &async_channel::Sender<Event>, dev: &mut Device) {
    dev.conn = None;
    dev.next_attempt = Instant::now() + RECONNECT_DELAY;
    if dev.connected {
        dev.connected = false;
        emit(events, dev);
    }
}

/// Open a TCP connection, negotiate a session key if the protocol version
/// needs one, and confirm the device answers a status query. On "auto",
/// failure advances to the next candidate version for the next attempt.
fn connect_device(events: &async_channel::Sender<Event>, dev: &mut Device) {
    dev.next_attempt = Instant::now() + RECONNECT_DELAY;
    let version = dev
        .version
        .unwrap_or(AUTO_VERSIONS[dev.probe_idx % AUTO_VERSIONS.len()]);
    match try_connect(dev, version) {
        Ok(conn) => {
            if dev.version.is_none() {
                eprintln!(
                    "Tuya {}: detected protocol {}",
                    dev.cfg.host,
                    version.as_str()
                );
            }
            dev.version = Some(version);
            dev.conn = Some(conn);
            dev.last_ok = Instant::now();
            if !dev.connected {
                dev.connected = true;
                emit(events, dev);
            }
        }
        Err(e) => {
            eprintln!(
                "Tuya {}: connect ({}) failed: {e}",
                dev.cfg.host,
                version.as_str()
            );
            if dev.version.is_none() {
                dev.probe_idx += 1;
                // Try the next candidate soon rather than in 5 s.
                dev.next_attempt = Instant::now() + Duration::from_millis(500);
            }
            if dev.connected {
                dev.connected = false;
                emit(events, dev);
            }
        }
    }
}

fn try_connect(dev: &mut Device, version: Version) -> Result<Conn, String> {
    let ip: Ipv4Addr = dev
        .cfg
        .host
        .trim()
        .parse()
        .map_err(|_| "invalid IP address".to_string())?;
    probe(ip, dev.cfg.id.trim(), &dev.key(), version)
}

/// Connect to `ip` and prove `key`/`version` fit the device there: via the
/// session-key handshake on 3.4/3.5, via a decryptable status reply on 3.3.
fn probe(ip: Ipv4Addr, devid: &str, key: &[u8; 16], version: Version) -> Result<Conn, String> {
    let addr = SocketAddr::from((ip, 6668));
    let stream =
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| e.to_string())?;

    let mut conn = Conn {
        stream,
        codec: Codec {
            version,
            key: *key,
        },
        seqno: 0,
        rbuf: Vec::new(),
    };

    if version != Version::V33 {
        // Session-key negotiation (3.4/3.5). Success proves both the key and
        // the protocol version.
        let local_nonce = protocol::random_local_nonce();
        conn.send(protocol::SESS_KEY_NEG_START, &local_nonce)
            .map_err(|e| e.to_string())?;
        let msg = wait_for(&mut conn, protocol::SESS_KEY_NEG_RESP)?;
        let remote_nonce = protocol::verify_neg_resp(key, &local_nonce, &msg.payload)
            .ok_or("session key response failed verification (wrong local key?)")?;
        conn.send(
            protocol::SESS_KEY_NEG_FINISH,
            &protocol::hmac_sha256(key, &remote_nonce),
        )
        .map_err(|e| e.to_string())?;
        conn.codec.key = protocol::session_key(version, key, &local_nonce, &remote_nonce);
        Ok(conn)
    } else {
        // 3.3 has no handshake; prove key+version with a status query.
        let json = format!(
            r#"{{"gwId":"{devid}","devId":"{devid}","uid":"{devid}","t":"{}"}}"#,
            epoch()
        );
        conn.send(protocol::DP_QUERY, json.as_bytes())
            .map_err(|e| e.to_string())?;
        let msg = wait_for(&mut conn, protocol::DP_QUERY)?;
        if !msg.payload.starts_with(b"{") {
            return Err("unexpected status reply (wrong protocol version?)".into());
        }
        Ok(conn)
    }
}

/// Scan `subnets` for hosts with TCP 6668 open, then identify which of
/// `candidates` lives at each host by seeing whose local key fits.
fn locate(
    events: &async_channel::Sender<Event>,
    candidates: Vec<TuyaConfig>,
    subnets: Vec<Subnet>,
) {
    let hosts: Vec<Ipv4Addr> = subnets.iter().flat_map(|s| s.hosts()).collect();
    let open = scan_port_6668(hosts);
    let mut unmatched = candidates;
    let mut found = 0usize;
    for host in open {
        let matched = unmatched.iter().position(|cand| {
            AUTO_VERSIONS.iter().any(|v| {
                probe(host, cand.id.trim(), &key16(&cand.key), *v)
                    .map(|_| {
                        let _ = events.send_blocking(Event::TuyaFound {
                            id: cand.id.trim().to_string(),
                            host: host.to_string(),
                            version: v.as_str().to_string(),
                        });
                    })
                    .is_ok()
            })
        });
        if let Some(i) = matched {
            unmatched.remove(i);
            found += 1;
        }
        if unmatched.is_empty() {
            break;
        }
    }
    let _ = events.send_blocking(Event::TuyaLocateDone { found });
}

/// Parallel TCP connect scan for port 6668 over up to 1024 hosts per subnet.
fn scan_port_6668(hosts: Vec<Ipv4Addr>) -> Vec<Ipv4Addr> {
    use std::sync::{Arc, Mutex};
    let queue = Arc::new(Mutex::new(hosts));
    let open = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..48)
        .map(|_| {
            let queue = queue.clone();
            let open = open.clone();
            std::thread::spawn(move || loop {
                let Some(ip) = queue.lock().unwrap().pop() else {
                    return;
                };
                let addr = SocketAddr::from((ip, 6668));
                if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() {
                    open.lock().unwrap().push(ip);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let mut result = std::mem::take(&mut *open.lock().unwrap());
    result.sort();
    result
}

/// Synchronously read frames until one with the wanted command arrives.
fn wait_for(conn: &mut Conn, cmd: u32) -> Result<protocol::Msg, String> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(msg) = conn.codec.decode_next(&mut conn.rbuf)? {
            if msg.cmd == cmd {
                return Ok(msg);
            }
            continue;
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for reply".into());
        }
        match conn.stream.read(&mut chunk) {
            Ok(0) => return Err("connection closed by device".into()),
            Ok(n) => conn.rbuf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err("timed out waiting for reply".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Drain incoming frames on all connections; returns whether anything was
/// read (used to decide if the main loop should sleep).
fn read_connections(
    events: &async_channel::Sender<Event>,
    devices: &mut HashMap<String, Device>,
) -> bool {
    let mut any = false;
    for dev in devices.values_mut() {
        let Some(conn) = dev.conn.as_mut() else { continue };
        let _ = conn.stream.set_read_timeout(Some(Duration::from_millis(50)));
        let mut chunk = [0u8; 2048];
        let mut dead = false;
        match conn.stream.read(&mut chunk) {
            Ok(0) => dead = true,
            Ok(n) => {
                any = true;
                conn.rbuf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => dead = true,
        }
        if dead {
            drop_connection(events, dev);
            continue;
        }
        while let Some(conn) = dev.conn.as_mut() {
            match conn.codec.decode_next(&mut conn.rbuf) {
                Ok(Some(msg)) => handle_msg(events, dev, msg),
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Tuya {}: protocol error: {e}", dev.cfg.host);
                    drop_connection(events, dev);
                    break;
                }
            }
        }
    }
    any
}

fn handle_msg(events: &async_channel::Sender<Event>, dev: &mut Device, msg: protocol::Msg) {
    dev.last_ok = Instant::now();
    let was_connected = dev.connected;
    dev.connected = true;
    if msg.retcode != 0 {
        eprintln!(
            "Tuya {}: command {} rejected (return code {})",
            dev.cfg.host, msg.cmd, msg.retcode
        );
    }
    let json: Option<serde_json::Value> = serde_json::from_slice(&msg.payload).ok();
    let mut changed = !was_connected;
    if let Some(dps) = json.as_ref().and_then(dps_of) {
        if dev.switch_dp.is_none() {
            dev.switch_dp = pick_switch_dp(dps);
        }
        // Merge (status pushes may carry only the changed data points).
        let dps_changed = dps
            .iter()
            .any(|(k, v)| dev.last_dps.get(k) != Some(v));
        for (k, v) in dps {
            dev.last_dps.insert(k.clone(), v.clone());
        }
        changed |= dps_changed;
        if Instant::now() >= dev.suppress_until {
            if let Some(on) = dev
                .switch_dp
                .as_ref()
                .and_then(|dp| dps.get(dp))
                .and_then(|v| v.as_bool())
            {
                changed |= on != dev.powered;
                dev.powered = on;
            }
        }
    }
    if changed {
        emit(events, dev);
    }
}

/// The "dps" map of a status payload, at the top level or under "data".
fn dps_of(v: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    v.get("dps")
        .or_else(|| v.get("data").and_then(|d| d.get("dps")))
        .and_then(|d| d.as_object())
}

/// The switch data point: dp "1" if it's a boolean, else the lowest-numbered
/// boolean dp (multi-gang strips start at "1" too).
fn pick_switch_dp(dps: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if dps.get("1").is_some_and(|v| v.is_boolean()) {
        return Some("1".to_string());
    }
    dps.iter()
        .filter(|(_, v)| v.is_boolean())
        .min_by_key(|(k, _)| k.parse::<u32>().unwrap_or(u32::MAX))
        .map(|(k, _)| k.clone())
}

/// Passive UDP discovery: refresh the IP/version of configured devices from
/// their broadcasts (also works for the plaintext port-6666 variant).
fn listen_udp(sockets: &[UdpSocket], devices: &mut HashMap<String, Device>) {
    let mut buf = [0u8; 2048];
    for socket in sockets {
        while let Ok((n, _)) = socket.recv_from(&mut buf) {
            let Some(info) = protocol::parse_udp(&buf[..n]) else {
                continue;
            };
            let Some(gwid) = info.get("gwId").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(dev) = devices.get_mut(gwid) else {
                continue;
            };
            if let Some(ip) = info.get("ip").and_then(|v| v.as_str()) {
                if dev.cfg.host.trim() != ip {
                    dev.cfg.host = ip.to_string();
                    dev.conn = None;
                }
            }
            if dev.version.is_none() {
                dev.version = info
                    .get("version")
                    .and_then(|v| v.as_str())
                    .and_then(Version::parse);
            }
        }
    }
}
