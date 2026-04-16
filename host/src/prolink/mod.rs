//! Pro DJ Link protocol — constants, packet types, and zero-copy parsing.

pub mod discovery;
pub mod packets;
pub mod builder;
pub mod beat_listener;
pub mod metadata;
pub mod status_listener;
pub mod virtual_cdj;

// ── Magic header ──────────────────────────────────────────────────────────────

/// Every Pro DJ Link UDP packet starts with these 10 bytes ("Qspt1WmJOL").
pub const MAGIC: [u8; 10] = [0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c];

// ── Ports ─────────────────────────────────────────────────────────────────────

pub const PORT_DISCOVERY: u16 = 50000; // device announce / keep-alive
pub const PORT_BEAT: u16 = 50001;      // beat packets, fader-start, on-air
pub const PORT_STATUS: u16 = 50002;    // CDJ / mixer detailed status

// ── Packet type byte (offset 0x0a) ────────────────────────────────────────────

// Port 50000 types
pub const PKT_ANNOUNCE: u8 = 0x0a;       // initial device announcement
pub const PKT_CLAIM1: u8 = 0x00;         // first-stage channel claim
pub const PKT_CLAIM2: u8 = 0x02;         // second-stage channel claim
pub const PKT_CLAIM_FINAL: u8 = 0x04;    // final-stage channel claim
pub const PKT_KEEPALIVE: u8 = 0x06;      // keep-alive (every ~1500 ms)
pub const PKT_CONFLICT: u8 = 0x08;       // channel number conflict

// Port 50001 types
pub const PKT_BEAT: u8 = 0x28;           // beat packet
pub const PKT_ABS_POSITION: u8 = 0x0b;  // CDJ-3000 absolute position

// Port 50002 types
pub const PKT_CDJ_STATUS: u8 = 0x0a;    // detailed CDJ status
pub const PKT_MIXER_STATUS: u8 = 0x29;  // mixer status

// ── Device types ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub const DEV_CDJ: u8 = 0x02;
#[allow(dead_code)]
pub const DEV_MIXER: u8 = 0x01;

// ── Special device numbers ────────────────────────────────────────────────────

/// Mixer is always device 0x21 (33).
#[allow(dead_code)]
pub const DN_MIXER: u8 = 0x21;

// ── Pitch encoding ────────────────────────────────────────────────────────────

/// Pitch value meaning 0% (normal speed) in beat/status packets.
pub const PITCH_NORMAL: u32 = 0x0010_0000;

/// Convert a 4-byte pitch field to a percentage (-100.0 … +100.0).
#[inline]
pub fn pitch_to_percent(raw: u32) -> f64 {
    (raw as f64 - PITCH_NORMAL as f64) / PITCH_NORMAL as f64 * 100.0
}

/// Convert a percentage to the 4-byte pitch field.
#[inline]
#[allow(dead_code)]
pub fn percent_to_pitch(pct: f64) -> u32 {
    ((pct / 100.0 + 1.0) * PITCH_NORMAL as f64) as u32
}

// ── BPM helpers ───────────────────────────────────────────────────────────────

/// No BPM available (track not loaded / not analysed).
pub const BPM_NONE: u16 = 0xFFFF;

/// Convert raw BPM field (BPM × 100) to f64.
#[inline]
pub fn bpm_from_raw(raw: u16) -> Option<f64> {
    if raw == BPM_NONE {
        None
    } else {
        Some(raw as f64 / 100.0)
    }
}

/// Compute effective BPM from track BPM and pitch field.
#[inline]
pub fn effective_bpm(track_bpm: f64, pitch_raw: u32) -> f64 {
    track_bpm * pitch_raw as f64 / PITCH_NORMAL as f64
}

// ── Beat constants ────────────────────────────────────────────────────────────

/// Beat counter unavailable.
#[allow(dead_code)]
pub const BEAT_NONE: u32 = 0xFFFF_FFFF;

// ── Debug helpers ─────────────────────────────────────────────────────────────

/// Format the first `n` bytes of a packet as a hex string for TRACE logging.
pub fn hex_preview(data: &[u8], n: usize) -> String {
    data.iter()
        .take(n)
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(dead_code)]
pub fn create_reuse_socket(port: u16) -> anyhow::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    use std::net::{Ipv4Addr, SocketAddrV4};

    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_reuse_address(true)?;
    #[cfg(not(windows))]
    raw.set_reuse_port(true)?;
    raw.set_nonblocking(true)?;
    raw.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(std::net::UdpSocket::from(raw))
}
