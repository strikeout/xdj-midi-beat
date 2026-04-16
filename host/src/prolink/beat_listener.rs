//! Beat listener — UDP 50001.
//!
//! Handles:
//! - Beat packets (type 0x28): fired on every beat by CDJs playing an
//!   analysed track, and periodically by the mixer.
//! - Absolute-position packets (type 0x0b): CDJ-3000 / XDJ-XZ only, every
//!   ~30 ms; preferred for phase tracking.

use std::net::{Ipv4Addr, SocketAddrV4};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use super::{
    packets::{parse_abs_position, parse_beat, AbsPositionPacket, BeatPacket},
    PORT_BEAT,
};

// ── Beat events ───────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum BeatEvent {
    /// Pro DJ Link beat packet (from hardware CDJ/XDJ on Ethernet).
    Beat(BeatPacket),
    /// Pro DJ Link absolute-position packet (CDJ-3000 only, every ~30ms).
    AbsPosition(AbsPositionPacket),
    /// Ableton Link beat crossing (from rekordbox or other Link peers).
    LinkBeat {
        bpm: f64,
        beat_in_bar: u8,
        bar_phase: f64,
        beat_phase: f64,
    },
}

// ── Listener task ─────────────────────────────────────────────────────────────

pub async fn run(
    _bind_ip: Ipv4Addr,
    tx: broadcast::Sender<BeatEvent>,
) -> anyhow::Result<()> {
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_reuse_address(true)?;
    #[cfg(not(windows))]
    raw.set_reuse_port(true)?;
    raw.set_nonblocking(true)?;
    raw.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT_BEAT).into())?;
    let sock = UdpSocket::from_std(std::net::UdpSocket::from(raw))?;

    tracing::info!(port = PORT_BEAT, "Beat listener started");

    let mut buf = [0u8; 2048];
    loop {
        match sock.recv_from(&mut buf).await {
            Ok((len, _src)) => {
                let data = &buf[..len];
                tracing::trace!(
                    len,
                    hex = %super::hex_preview(data, 48),
                    "Beat packet received"
                );
                if let Some(bp) = parse_beat(data) {
                    let _ = tx.send(BeatEvent::Beat(bp));
                } else if let Some(ap) = parse_abs_position(data) {
                    let _ = tx.send(BeatEvent::AbsPosition(ap));
                }
            }
            Err(e) => tracing::warn!("Beat listener recv error: {e}"),
        }
    }
}
