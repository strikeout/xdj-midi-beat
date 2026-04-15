//! Virtual CDJ — joins the Pro DJ Link network so real hardware sends us
//! detailed status packets on UDP port 50002.
//!
//! Implements the full 4-stage channel-claim sequence, then broadcasts
//! keep-alive packets every 1500 ms.  Uses CDJ-3000-compatible byte 0x35 = 0x64
//! to avoid disrupting players on channels 5 and 6.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use super::discovery::DeviceTable;
use super::{
    MAGIC, PKT_ANNOUNCE, PKT_CDJ_STATUS, PKT_CLAIM1, PKT_CLAIM2, PKT_CLAIM_FINAL, PKT_CONFLICT,
    PKT_KEEPALIVE, PORT_DISCOVERY, PORT_STATUS,
};

// ── Timing constants ──────────────────────────────────────────────────────────

const CLAIM_INTERVAL: Duration = Duration::from_millis(300);
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(1500);
const CLAIM_ROUNDS: usize = 3;

// ── Signals ───────────────────────────────────────────────────────────────────

/// Sent once the virtual CDJ has successfully claimed a channel.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct VirtualCdjReady {
    pub device_number: u8,
    pub ip: [u8; 4],
    pub mac: [u8; 6],
}

// ── Packet builders ───────────────────────────────────────────────────────────

fn pad_name(name: &str) -> [u8; 20] {
    let mut buf = [0u8; 20];
    let bytes = name.as_bytes();
    let len = bytes.len().min(19); // leave room for null terminator
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// Common keep-alive packet header (bytes 0x00 – 0x23).
/// All keep-alive packet types share this layout:
///   0x00-0x09: magic
///   0x0a:      type
///   0x0b:      padding
///   0x0c-0x1f: model (20 bytes CString, null-padded)
///   0x20:      u1 = 0x01
///   0x21:      device_type (CDJ = 0x02)
///   0x22:      padding
///   0x23:      subtype
fn write_keepalive_header(p: &mut [u8], pkt_type: u8, subtype: u8, name: &[u8; 20]) {
    p[..10].copy_from_slice(&MAGIC);
    p[0x0a] = pkt_type;
    // 0x0b = 0x00 (padding, already zero)
    p[0x0c..0x20].copy_from_slice(name);
    p[0x20] = 0x01;       // u1
    p[0x21] = 0x04;       // device_type: Rekordbox = 0x04
    // 0x22 = 0x00 (padding)
    p[0x23] = subtype;
}

// type_hello (0x0a), subtype 0x25.  Content: 1 byte (u2=1).
fn build_announce(name: &[u8; 20]) -> [u8; 0x25] {
    let mut p = [0u8; 0x25];
    write_keepalive_header(&mut p, PKT_ANNOUNCE, 0x25, name);
    p[0x24] = 0x01; // u2 (CDJs send 1, DJMs send 3)
    p
}

// type_mac (0x00), subtype 0x2c.
// Content: iteration(1) + flags(1) + mac(6) = 8 bytes.
fn build_claim1(name: &[u8; 20], mac: &[u8; 6], n: u8) -> [u8; 0x2c] {
    let mut p = [0u8; 0x2c];
    write_keepalive_header(&mut p, PKT_CLAIM1, 0x2c, name);
    p[0x24] = n;    // iteration (1..3)
    p[0x25] = 0x01; // flags: is_player_or_mixer
    p[0x26..0x2c].copy_from_slice(mac);
    p
}

// type_ip (0x02), subtype 0x32.
// Content: ip(4) + mac(6) + player_number(1) + iteration(1) + flags(1) + assignment(1) = 14 bytes.
fn build_claim2(name: &[u8; 20], ip: &[u8; 4], mac: &[u8; 6], device_num: u8, n: u8) -> [u8; 0x32] {
    let mut p = [0u8; 0x32];
    write_keepalive_header(&mut p, PKT_CLAIM2, 0x32, name);
    p[0x24..0x28].copy_from_slice(ip);
    p[0x28..0x2e].copy_from_slice(mac);
    p[0x2e] = device_num;
    p[0x2f] = n;    // iteration (1..3)
    p[0x30] = 0x01; // flags: is_player_or_mixer
    p[0x31] = 0x01; // player_number_assignment: auto
    p
}

// type_number (0x04), subtype 0x26.
// Content: proposed_player_number(1) + iteration(1) = 2 bytes.
fn build_claim_final(name: &[u8; 20], device_num: u8, n: u8) -> [u8; 0x26] {
    let mut p = [0u8; 0x26];
    write_keepalive_header(&mut p, PKT_CLAIM_FINAL, 0x26, name);
    p[0x24] = device_num; // proposed_player_number
    p[0x25] = n;          // iteration (1..3)
    p
}

fn build_keepalive(
    name: &[u8; 20],
    device_num: u8,
    mac: &[u8; 6],
    ip: &[u8; 4],
    _peers: u8,
) -> [u8; 0x36] {
    let mut p = [0u8; 0x36];
    p[..10].copy_from_slice(&MAGIC);
    p[0x0a] = PKT_KEEPALIVE;
    // 0x0b = 0x00 (padding)
    // 0x0c-0x1f: model name — 20 bytes, CString null-padded.
    // Place the name starting at 0x0c (matching python-prodj-link layout).
    let name_len = name.len().min(19); // leave room for null terminator within 20 bytes
    p[0x0c..0x0c + name_len].copy_from_slice(&name[..name_len]);
    // Bytes 0x0c+name_len through 0x1f are already zero (null padding).
    p[0x20] = 0x01;       // u1 (constant)
    p[0x21] = 0x04;       // device_type: Rekordbox = 0x04
    // 0x22 = 0x00 (padding)
    p[0x23] = 0x36;       // subtype: stype_status
    // --- content for type_status ---
    p[0x24] = device_num; // player_number
    p[0x25] = 0x04;       // u2 (device_type: Rekordbox = 0x04)
    p[0x26..0x2c].copy_from_slice(mac);
    p[0x2c..0x30].copy_from_slice(ip);
    p[0x30] = 0x01;       // device_count = 1 (matching prolink-go)
    // 0x31-0x33 = 0x00 (padding, 3 bytes)
    p[0x34] = 0x04;       // flags: is_player_or_mixer = 1 (device_type: Rekordbox = 0x04)
    p[0x35] = 0x00;       // u4: 0x00 (matching prolink-go, drop CDJ-3000 flag)
    p
}

/// Build a CDJ status packet (port 50002) to unicast to each real device.
///
/// This is a mostly-static template matching prolink-go's `getStatusPacket`.
/// Real CDJs need to receive this before they will share detailed metadata
/// (especially for unanalysed mp3 files) and to avoid "older firmware" warnings.
///
/// Notable bytes (from prolink-go comments):
///   0x68 = 0x01 and 0x75 = 0x01  — required for CDJ mp3 metadata
///   0xb6 = 0x01                   — avoids "older firmware" warning
fn build_status_packet(name: &[u8; 20], device_num: u8) -> [u8; 0x11c] {
    // Template from prolink-go's getStatusPacket, 284 bytes (0x11c).
    #[rustfmt::skip]
    let mut b: [u8; 0x11c] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x03, 0x00, 0x00, 0xf8, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x04, 0x04, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9c, 0xff, 0xfe, 0x00, 0x10, 0x00, 0x00,
        0x7f, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0xff, 0xff, 0xff, 0xff, 0x01, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    // Overlay the ProLink header (10 bytes at 0x00).
    b[..10].copy_from_slice(&MAGIC);
    // Byte 0x0a is already 0x0a (PKT_CDJ_STATUS) from the template.
    debug_assert_eq!(b[0x0a], PKT_CDJ_STATUS);
    // Device name at 0x0b (20 bytes, matching prolink-go).
    b[0x0b..0x0b + 20].copy_from_slice(name);
    // Device ID at 0x21 and 0x24 (prolink-go sets both).
    b[0x21] = device_num;
    b[0x24] = device_num;
    // Firmware at 0x7c ("1.43", 4 bytes).
    b[0x7c..0x80].copy_from_slice(b"1.43");
    b
}

// ── Main task ─────────────────────────────────────────────────────────────────

/// Run the virtual CDJ: claim a channel number, then broadcast keep-alive
/// packets forever.
///
/// Fires `ready_tx` once the claim sequence succeeds so downstream tasks
/// (status listener, etc.) know they can start.
pub async fn run(
    bind_ip: Ipv4Addr,
    broadcast_ip: Ipv4Addr,
    mac: [u8; 6],
    device_number: u8,
    device_name: &str,
    device_table: DeviceTable,
    ready_tx: broadcast::Sender<VirtualCdjReady>,
) -> anyhow::Result<()> {
    use super::packets::has_magic;

    let ip: [u8; 4] = bind_ip.octets();
    let name = pad_name(device_name);
    let bcast_addr = SocketAddrV4::new(broadcast_ip, PORT_DISCOVERY);
    let bcast_addr6: std::net::SocketAddr = bcast_addr.into();

    // Build the discovery socket
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_reuse_address(true)?;
    #[cfg(not(windows))]
    raw.set_reuse_port(true)?;
    raw.set_broadcast(true)?;
    raw.set_nonblocking(true)?;
    raw.bind(&SocketAddrV4::new(bind_ip, PORT_DISCOVERY).into())?;
    let sock = UdpSocket::from_std(std::net::UdpSocket::from(raw))?;
    let sock = Arc::new(sock);

    // ── Stage 1: Initial announcements ───────────────────────────────────────
    tracing::info!(device = device_number, "Virtual CDJ: sending announcements");
    let announce_pkt = build_announce(&name);
    for _ in 0..CLAIM_ROUNDS {
        tracing::trace!(
            len = announce_pkt.len(),
            hex = %super::hex_preview(&announce_pkt, 48),
            "Sending announce packet"
        );
        sock.send_to(&announce_pkt, bcast_addr6).await?;
        tokio::time::sleep(CLAIM_INTERVAL).await;
    }

    // ── Stage 2: First-stage claim ────────────────────────────────────────────
    tracing::info!(device = device_number, "Virtual CDJ: first-stage claim");
    for n in 1..=(CLAIM_ROUNDS as u8) {
        let pkt = build_claim1(&name, &mac, n);
        tracing::trace!(
            len = pkt.len(),
            iteration = n,
            hex = %super::hex_preview(&pkt, 48),
            "Sending claim1 packet"
        );
        sock.send_to(&pkt, bcast_addr6).await?;
        tokio::time::sleep(CLAIM_INTERVAL).await;
    }

    // ── Stage 3: Second-stage claim + conflict detection ──────────────────
    tracing::info!(device = device_number, "Virtual CDJ: second-stage claim");
    let mut claimed = device_number;
    let mut buf = [0u8; 256];
    for n in 1..=(CLAIM_ROUNDS as u8) {
        let pkt = build_claim2(&name, &ip, &mac, claimed, n);
        tracing::trace!(
            len = pkt.len(),
            iteration = n,
            hex = %super::hex_preview(&pkt, 48),
            "Sending claim2 packet"
        );
        sock.send_to(&pkt, bcast_addr6).await?;
        // Listen briefly for conflict packets.
        let deadline = tokio::time::Instant::now() + CLAIM_INTERVAL;
        loop {
            let sleep = tokio::time::sleep_until(deadline);
            tokio::select! {
                _ = sleep => break,
                result = sock.recv_from(&mut buf) => {
                    if let Ok((len, _)) = result {
                        let data = &buf[..len];
                        if has_magic(data) && data[0x0a] == PKT_CONFLICT && data[0x24] == claimed {
                            // Our number is taken; increment and restart stage 3.
                            claimed += 1;
                            if claimed > 15 {
                                anyhow::bail!("No free device numbers available");
                            }
                            tracing::warn!(
                                old = claimed - 1,
                                new = claimed,
                                "Device number conflict, retrying"
                            );
                        }
                    }
                }
            }
        }
    }

    // ── Stage 4: Final claim ──────────────────────────────────────────────────
    tracing::info!(device = claimed, "Virtual CDJ: final claim");
    for n in 1..=(CLAIM_ROUNDS as u8) {
        let pkt = build_claim_final(&name, claimed, n);
        tracing::trace!(
            len = pkt.len(),
            iteration = n,
            hex = %super::hex_preview(&pkt, 48),
            "Sending claim_final packet"
        );
        sock.send_to(&pkt, bcast_addr6).await?;
        tokio::time::sleep(CLAIM_INTERVAL).await;
    }

    tracing::info!(device = claimed, ip = ?ip, "Virtual CDJ online");
    let _ = ready_tx.send(VirtualCdjReady {
        device_number: claimed,
        ip,
        mac,
    });

    let status_pkt = build_status_packet(&name, claimed);
    let table = Arc::clone(&device_table);
    let listener_sock = Arc::clone(&sock);
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            match listener_sock.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    let data = &buf[..len];
                    if let Some(ka) = super::packets::parse_keepalive(data) {
                        let dev = super::discovery::Device::from_keepalive(ka);
                        let num = dev.device_number;
                        let mut t = table.lock();
                        let is_new = !t.contains_key(&num);
                        if is_new {
                            tracing::info!(
                                device = num,
                                name = %dev.name,
                                ip = ?dev.ip,
                                "Virtual CDJ: discovered real CDJ on network"
                            );
                        }
                        t.insert(num, dev);
                    }
                    let status_addr: std::net::SocketAddr = match src.ip() {
                        std::net::IpAddr::V4(v4) => SocketAddrV4::new(v4, PORT_STATUS).into(),
                        std::net::IpAddr::V6(_) => continue,
                    };
                    if let Err(e) = listener_sock.send_to(&status_pkt, status_addr).await {
                        tracing::debug!("Status send error to {status_addr}: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Virtual CDJ listener recv error: {e}");
                }
            }
        }
    });

    // ── Keep-alive loop ───────────────────────────────────────────────────────
    let status_pkt = build_status_packet(&name, claimed);
    let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
    loop {
        interval.tick().await;

        // 1. Broadcast keep-alive on port 50000 using the interface subnet broadcast
        //    so it routes out the correct physical network interface.
        let pkt = build_keepalive(&name, claimed, &mac, &ip, 1);
        tracing::trace!(
            len = pkt.len(),
            hex = %super::hex_preview(&pkt, 48),
            "Sending keepalive packet"
        );
        if let Err(e) = sock.send_to(&pkt, bcast_addr6).await {
            tracing::warn!("Keep-alive send error: {e}");
        }

        // 2. Unicast status packet to each known device on port 50002.
        //    Real CDJs require this to share detailed metadata and to avoid
        //    "older firmware" warnings (matching prolink-go behaviour).
        {
            let devices: Vec<[u8; 4]> = device_table
                .lock()
                .values()
                .map(|d| d.ip)
                .collect();
            for dev_ip in devices {
                let addr: std::net::SocketAddr =
                    SocketAddrV4::new(Ipv4Addr::from(dev_ip), PORT_STATUS).into();
                match sock.send_to(&status_pkt, addr).await {
                    Ok(n) => {
                        tracing::trace!(
                            target = %addr,
                            len = status_pkt.len(),
                            sent = n,
                            "Status unicast sent"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target = %addr,
                            error = %e,
                            "Status unicast FAILED"
                        );
                    }
                }
            }
        }
    }
}
