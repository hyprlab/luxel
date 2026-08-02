//! Quick LAN discovery probe: `cargo run --example discover`
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use lifx_core::{BuildOptions, Message, RawMessage};

fn main() -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;

    let opts = BuildOptions {
        target: None,
        ack_required: false,
        res_required: true,
        sequence: 0,
        source: 0x0BAD_CAFE,
    };
    let raw = RawMessage::build(&opts, Message::GetService).unwrap();
    socket.send_to(&raw.pack().unwrap(), (Ipv4Addr::BROADCAST, 56700))?;
    println!("broadcast sent, listening 3s…");

    let start = Instant::now();
    let mut buf = [0u8; 1024];
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok((n, src)) = socket.recv_from(&mut buf) {
            if let Ok(raw) = RawMessage::unpack(&buf[..n]) {
                if let Ok(msg) = Message::from_raw(&raw) {
                    println!("{src}: target={:012x} {msg:?}", raw.frame_addr.target);
                }
            }
        }
    }
    Ok(())
}
