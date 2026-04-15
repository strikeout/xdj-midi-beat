//! xdj-clock — Pioneer CDJ/XDJ Pro DJ Link → MIDI Clock/CC/Note bridge.
//!
//! Usage:
//!   xdj-clock [--config config.toml] [--interface eth0] [--midi "IAC Bus 1"]
//!             [--source prolink|link|auto] [--list-midi] [--device-number 7]
//!             [--log-level debug] [--no-tui]

#![warn(clippy::all)]

mod config;
mod link;
mod midi;
mod prolink;
mod state;
mod tui;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use midir::{MidiOutput, MidiOutputConnection};
use network_interface::{NetworkInterface, NetworkInterfaceConfig, V4IfAddr};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::prolink::{MAGIC, PKT_KEEPALIVE, PORT_DISCOVERY};

use crate::prolink::beat_listener::BeatEvent;
use crate::prolink::status_listener::StatusEvent;
use crate::prolink::virtual_cdj::VirtualCdjReady;
use crate::state::SharedState;
use crate::tui::state::{LogBuffer, MakeLogWriter};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "xdj-clock", version, about = "Pioneer CDJ/XDJ → MIDI bridge")]
struct Cli {
    /// Path to TOML config file.
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Override network interface (e.g. "Ethernet", "en0").
    #[arg(short, long)]
    interface: Option<String>,

    /// Override MIDI output port name (substring match).
    #[arg(short, long)]
    midi: Option<String>,

    /// Override virtual CDJ device number (1–15).
    #[arg(short, long)]
    device_number: Option<u8>,

    /// Beat source: prolink (Ethernet CDJs), link (Ableton Link / rekordbox), auto.
    #[arg(long)]
    source: Option<String>,

    /// Log level: error, warn, info, debug, trace.
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// List available MIDI output ports and exit.
    #[arg(long)]
    list_midi: bool,

    /// List detected network interfaces with priority ranking and exit.
    #[arg(long)]
    list_interfaces: bool,

    /// Disable the TUI and use plain log output.
    #[arg(long)]
    no_tui: bool,
}

// ── Startup helpers ───────────────────────────────────────────────────────────

/// Detect the network interface to use for Pro DJ Link.
/// Returns (bind_ip, broadcast_ip, mac_address).
///
/// When `hint` is `"auto"`, all non-loopback IPv4 interfaces are ranked by
/// priority so that Pioneer/AlphaTheta adapters are preferred over physical
/// Ethernet, which is preferred over Wi-Fi, while VPN and virtual adapters
/// are deprioritized.  When `hint` is anything else, only interfaces whose
/// name contains that substring (case-insensitive) are considered.
fn detect_interface(hint: &str) -> anyhow::Result<(Ipv4Addr, Ipv4Addr, [u8; 6])> {
    let ifaces = NetworkInterface::show()?;

    // Collect every usable (non-loopback IPv4) candidate with its priority.
    struct Candidate {
        name: String,
        ip: Ipv4Addr,
        bcast: Ipv4Addr,
        mac: [u8; 6],
        priority: u8, // lower = better
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    for iface in &ifaces {
        // If the user gave an explicit hint, filter by name substring.
        if hint != "auto" && !iface.name.to_lowercase().contains(&hint.to_lowercase()) {
            continue;
        }
        for addr in &iface.addr {
            if let network_interface::Addr::V4(V4IfAddr { ip, broadcast, .. }) = addr {
                if ip.is_loopback() {
                    continue;
                }

                let bcast = broadcast.unwrap_or_else(|| {
                    let o = ip.octets();
                    Ipv4Addr::new(o[0], o[1], o[2], 255)
                });

                let mac = iface
                    .mac_addr
                    .as_ref()
                    .and_then(|mac_str| {
                        let bytes: Vec<u8> = mac_str
                            .split(':')
                            .filter_map(|s| u8::from_str_radix(s, 16).ok())
                            .collect();
                        <[u8; 6]>::try_from(bytes.as_slice()).ok()
                    })
                    .unwrap_or([0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x02]);

                let priority = interface_priority(&iface.name, ip);

                tracing::debug!(
                    name = %iface.name,
                    %ip,
                    %bcast,
                    priority,
                    "Discovered network interface"
                );

                candidates.push(Candidate {
                    name: iface.name.clone(),
                    ip: *ip,
                    bcast,
                    mac,
                    priority,
                });
            }
        }
    }

    // Sort: lowest priority number wins; ties broken by name for determinism.
    candidates.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.name.cmp(&b.name)));

    if hint == "auto" {
        let best_priority = candidates.first().map(|c| c.priority);
        let tied_best: Vec<_> = candidates
            .iter()
            .take_while(|c| Some(c.priority) == best_priority)
            .collect();

        if tied_best.len() > 1 {
            tracing::info!(
                priority = tied_best[0].priority,
                count = tied_best.len(),
                "Multiple equal-priority interfaces found; probing for Pro DJ Link keepalives"
            );

            for cand in tied_best {
                tracing::debug!(name = %cand.name, ip = %cand.ip, "Probing interface for keepalives");
                if interface_has_keepalive(cand.ip, cand.bcast) {
                    if cand.mac == [0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x02] {
                        tracing::warn!("Could not detect MAC address for {}, using placeholder", cand.name);
                    }
                    tracing::info!(
                        name = %cand.name,
                        ip = %cand.ip,
                        bcast = %cand.bcast,
                        priority = cand.priority,
                        "Selected network interface"
                    );
                    return Ok((cand.ip, cand.bcast, cand.mac));
                }
            }

            tracing::info!(
                "No keepalives detected on equal-priority interfaces; falling back to the first candidate"
            );
        }
    }

    if let Some(best) = candidates.first() {
        if best.mac == [0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x02] {
            tracing::warn!("Could not detect MAC address for {}, using placeholder", best.name);
        }
        tracing::info!(
            name = %best.name,
            ip = %best.ip,
            bcast = %best.bcast,
            priority = best.priority,
            "Selected network interface"
        );
        return Ok((best.ip, best.bcast, best.mac));
    }

    anyhow::bail!(
        "No suitable network interface found. \
         Available: {:?}. Use --interface to specify one.",
        ifaces.iter().map(|i| &i.name).collect::<Vec<_>>()
    )
}

fn interface_has_keepalive(ip: Ipv4Addr, bcast: Ipv4Addr) -> bool {
    use socket2::{Domain, Protocol, Socket, Type};
    let raw = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(%ip, error = %e, "Unable to create probe socket");
            return false;
        }
    };
    let _ = raw.set_reuse_address(true);
    #[cfg(not(windows))]
    let _ = raw.set_reuse_port(true);

    // Bind to the subnet broadcast address to ensure we receive broadcast keepalives on macOS.
    // Fallback to 0.0.0.0 if binding to broadcast fails (some OSes don't allow it).
    let socket = match raw.bind(&std::net::SocketAddrV4::new(bcast, PORT_DISCOVERY).into()).or_else(|_| raw.bind(&std::net::SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT_DISCOVERY).into())) {
        Ok(_) => std::net::UdpSocket::from(raw),
        Err(err) => {
            tracing::debug!(%ip, error = %err, "Unable to bind probe socket");
            return false;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; 2048];

    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }

        if let Err(err) = socket.set_read_timeout(Some(deadline.saturating_duration_since(now))) {
            tracing::debug!(%ip, error = %err, "Unable to set probe socket timeout");
            return false;
        }

        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                if len > 0x0a && buf[..10] == MAGIC && buf[0x0a] == PKT_KEEPALIVE {
                    tracing::info!(%ip, %peer, "Detected Pro DJ Link keepalive");
                    return true;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return false;
            }
            Err(err) => {
                tracing::debug!(%ip, error = %err, "Probe socket error");
                return false;
            }
        }
    }
}

/// Assign a priority to a network interface by name and IP (lower = better).
///
    /// The ranking is:
    ///   0 — Link-local (169.254.x.x) or Pioneer / AlphaTheta DJ adapters
    ///   1 — Physical Ethernet with routable IP
    ///   2 — Wi-Fi / wireless with routable IP
    ///   3 — Unknown / unrecognised (still usable)
    ///   5 — VPN / virtual / tunnels (deprioritised)
pub(crate) fn interface_priority(name: &str, ip: &Ipv4Addr) -> u8 {
    let lower = name.to_lowercase();

    // ── VPN / virtual / tunnel adapters (deprioritise) ───────────────────
    const VPN_KEYWORDS: &[&str] = &[
        "pangp",           // Palo Alto GlobalProtect
        "globalprotect",
        "vpn",
        "tunnel",
        "tun",
        "tap",
        "virtual",
        "vmware",
        "vmnet",
        "vethernet",
        "hyper-v",
        "docker",
        "vbox",
        "virtualbox",
        "wsl",
        "loopback",
        "npcap",
        "winpcap",
    ];
    for kw in VPN_KEYWORDS {
        if lower.contains(kw) {
            return 5;
        }
    }

    // ── Base priority from adapter type ──────────────────────────────────
    const WIFI_KEYWORDS: &[&str] = &["wi-fi", "wifi", "wlan", "wireless", "80211"];
    const ETH_KEYWORDS: &[&str] = &["ethernet", "eth", "en0", "en1", "enp", "eno"];

    let mut base = 3u8; // unknown
    for kw in WIFI_KEYWORDS {
        if lower.contains(kw) {
            base = 2;
            break;
        }
    }
    for kw in ETH_KEYWORDS {
        if lower.contains(kw) {
            base = 1;
            break;
        }
    }

    // ── Adjust by IP characteristics ─────────────────────────────────────
    let octets = ip.octets();

    // Link-local (169.254.x.x / APIPA) — Pro DJ Link often self-assigns these,
    // so they should be preferred over routable addresses.
    if octets[0] == 169 && octets[1] == 254 {
        return 0;
    }

    // ── Pioneer / AlphaTheta DJ adapters (highest priority) ──────────────
    const DJ_KEYWORDS: &[&str] = &[
        "alphatheta",
        "pioneer",
        "pro dj",
        "prodj",
        "rekordbox",
    ];
    for kw in DJ_KEYWORDS {
        if lower.contains(kw) {
            return 0;
        }
    }

    // Private LAN ranges (192.168.x.x, 10.x.x.x, 172.16-31.x.x) are good.
    // But 10.x.x.x on a generic "Ethernet" adapter is often a VPN.
    // Heuristic: if the adapter name is generic (just "Ethernet N") and
    // the IP is 10.x.x.x, it's likely a VPN overlay — deprioritise.
    if octets[0] == 10 && base == 1 {
        // Generic numbered Ethernet? Probably VPN.
        // Physical Ethernet is usually "Ethernet" (no number) on Windows,
        // or has a descriptive name.
        let stripped = lower.replace("ethernet", "").trim().to_string();
        if stripped.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
            return 5; // treat as VPN
        }
    }

    base
}

/// Open a MIDI output connection, matching `port_name` (substring, case-insensitive).
fn open_midi_output(port_name: &str) -> anyhow::Result<MidiOutputConnection> {
    let midi_out = MidiOutput::new("xdj-clock")?;
    let ports = midi_out.ports();
    if ports.is_empty() {
        anyhow::bail!("No MIDI output ports available");
    }

    if port_name == "auto" {
        let port = &ports[0];
        let name = midi_out.port_name(port)?;
        tracing::info!(%name, "Auto-selected MIDI output port");
        return midi_out.connect(port, "xdj-clock").map_err(|e| anyhow::anyhow!("{}", e));
    }

    for port in &ports {
        let name = midi_out.port_name(port)?;
        if name.to_lowercase().contains(&port_name.to_lowercase()) {
            tracing::info!(%name, "Selected MIDI output port");
            return midi_out.connect(port, "xdj-clock").map_err(|e| anyhow::anyhow!("{}", e));
        }
    }
    anyhow::bail!("MIDI port matching {:?} not found", port_name)
}

fn list_midi_ports() -> anyhow::Result<()> {
    let midi_out = MidiOutput::new("xdj-clock")?;
    let ports = midi_out.ports();
    if ports.is_empty() {
        println!("No MIDI output ports found.");
        return Ok(());
    }
    println!("Available MIDI output ports:");
    for (i, port) in ports.iter().enumerate() {
        println!("  [{i}] {}", midi_out.port_name(port)?);
    }
    Ok(())
}

fn list_interfaces() -> anyhow::Result<()> {
    let ifaces = NetworkInterface::show()?;
    let mut found = false;

    println!("Detected network interfaces:");
    println!("{:<6} {:<30} {:<18} {:<18} {}", "Prio", "Name", "IPv4", "Broadcast", "MAC");
    println!("{}", "-".repeat(95));

    for iface in &ifaces {
        for addr in &iface.addr {
            if let network_interface::Addr::V4(V4IfAddr { ip, broadcast, .. }) = addr {
                if ip.is_loopback() {
                    continue;
                }
                let bcast = broadcast
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "-".into());
                let mac_str = iface
                    .mac_addr
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "-".into());
                let prio = interface_priority(&iface.name, ip);
                let prio_label = match prio {
                    0 => "0 (DJ)",
                    1 => "1 (Eth)",
                    2 => "2 (WiFi)",
                    3 => "3 (?)",
                    4 => "4 (LnkL)",
                    5 => "5 (VPN)",
                    _ => "?",
                };
                println!("{:<6} {:<30} {:<18} {:<18} {}", prio_label, iface.name, ip, bcast, mac_str);
                found = true;
            }
        }
    }

    if !found {
        println!("  (no non-loopback IPv4 interfaces found)");
    }

    println!();
    println!("Priority: 0=Link-local/DJ, 1=Ethernet, 2=Wi-Fi, 3=Unknown, 5=VPN/Virtual");
    println!("Auto-select picks the lowest priority number. Use --interface to override.");
    Ok(())
}

// ── State applier tasks ───────────────────────────────────────────────────────

/// Consumes status events and applies them to shared state.
async fn status_applier(
    state: SharedState,
    cfg: config::SharedConfig,
    mut rx: broadcast::Receiver<StatusEvent>,
    track_change_tx: mpsc::Sender<crate::state::TrackChange>,
) {
    loop {
        match rx.recv().await {
            Ok(StatusEvent::Cdj(s)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                let change = state.apply_cdj_status(&s);
                drop(state); // release write lock before async send
                if let Some(tc) = change {
                    let _ = track_change_tx.try_send(tc);
                }
            }
            Ok(StatusEvent::Mixer(s)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                state.apply_mixer_status(&s);
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Status applier lagged, dropped {n} events");
            }
        }
    }
}

/// Consumes beat events and applies them to shared state.
async fn beat_applier(
    state: SharedState,
    cfg: config::SharedConfig,
    mut rx: broadcast::Receiver<BeatEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(BeatEvent::Beat(bp)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                state.apply_beat(&bp);
            }
            Ok(BeatEvent::AbsPosition(ap)) => {
                let smoothing_ms = cfg.read().midi.smoothing_ms;
                let mut state = state.write();
                state.set_smoothing_ms(smoothing_ms);
                state.apply_abs_position(&ap);
            }
            // LinkBeat: shared state is updated directly by the link engine;
            // nothing extra to do here.
            Ok(BeatEvent::LinkBeat { .. }) => {}
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Beat applier lagged, dropped {n} events");
            }
        }
    }
}

// ── Device event logger ───────────────────────────────────────────────────────

async fn device_logger(
    state: SharedState,
    mut rx: broadcast::Receiver<crate::prolink::discovery::DeviceEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(crate::prolink::discovery::DeviceEvent::Appeared(d)) => {
                tracing::info!(
                    device = d.device_number,
                    name = %d.name,
                    ip = ?d.ip,
                    "DJ device appeared on network"
                );
                state.write().mark_prolink_seen();
            }
            Ok(crate::prolink::discovery::DeviceEvent::Disappeared(num)) => {
                tracing::info!(device = num, "DJ device left network");
                state.write().remove_device(num);
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(_) => {}
        }
    }
}

// ── Headless status loop (--no-tui) ──────────────────────────────────────────

async fn headless_loop(
    state: SharedState,
    midi_conn: Arc<Mutex<Option<MidiOutputConnection>>>,
) -> anyhow::Result<()> {
    tracing::info!("All tasks running (headless mode). Ctrl+C to quit.");
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let master = state.read().master.clone();
                if master.device_number > 0 || master.source.is_some() {
                    tracing::info!(
                        source = ?master.source,
                        master_deck = master.device_number,
                        bpm = %format!("{:.2}", master.bpm),
                        pitch = %format!("{:+.2}%", master.pitch_pct),
                        beat = master.beat_in_bar,
                        playing = master.is_playing,
                        "Master status"
                    );
                } else {
                    tracing::info!("Waiting for tempo master…");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Shutting down…");
                if let Some(ref mut c) = *midi_conn.lock() {
                    let _ = c.send(&[0xFC]);
                }
                tracing::info!("MIDI Stop sent, exiting.");
                return Ok(());
            }
        }
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let use_tui = !cli.no_tui;

    // ── Logging ──────────────────────────────────────────────────────────────
    // When the TUI is active, redirect tracing to an in-memory ring buffer so
    // ratatui owns stdout.  In headless mode, log to stderr as before.
    let log_buf = LogBuffer::new();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::from_str(&cli.log_level).unwrap_or_default()
        });

    if use_tui {
        use tracing_subscriber::fmt;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .with_target(false)
                    .with_ansi(false)
                    .with_writer(MakeLogWriter::new(log_buf.clone())),
            )
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .init();
    }

    // ── MIDI port list shortcut ──────────────────────────────────────────────
    if cli.list_midi {
        return list_midi_ports();
    }
    if cli.list_interfaces {
        return list_interfaces();
    }

    // ── Config ───────────────────────────────────────────────────────────────
    let mut initial_cfg = config::load(&cli.config)?;
    if let Some(iface) = cli.interface {
        initial_cfg.interface = iface;
    }
    if let Some(midi) = cli.midi {
        initial_cfg.midi.output = midi;
    }
    if let Some(dn) = cli.device_number {
        initial_cfg.device_number = dn;
    }
    if let Some(src) = cli.source {
        initial_cfg.source = match src.to_lowercase().as_str() {
            "prolink" => config::Source::ProLink,
            "link" => config::Source::Link,
            "auto" => config::Source::Auto,
            other => anyhow::bail!("Unknown --source {:?}: use prolink, link, or auto", other),
        };
    }

    let cfg = config::new_shared(initial_cfg);

    let startup_cfg = cfg.read().clone();

    tracing::info!(
        source = ?startup_cfg.source,
        interface = %startup_cfg.interface,
        device_number = startup_cfg.device_number,
        device_name = %startup_cfg.device_name,
        midi_port = %startup_cfg.midi.output,
        "xdj-clock starting"
    );

    // ── Network interface ────────────────────────────────────────────────────
    let (bind_ip, bcast_ip, mac) = detect_interface(&startup_cfg.interface)?;

    // ── MIDI output (swappable — Option so port can be changed at runtime) ───
    let midi_conn: Arc<Mutex<Option<MidiOutputConnection>>> =
        match open_midi_output(&startup_cfg.midi.output) {
            Ok(conn) => Arc::new(Mutex::new(Some(conn))),
            Err(e) => {
                tracing::warn!("Could not open MIDI output: {e}");
                tracing::warn!("Running without MIDI — select a port in the TUI or restart with --midi");
                Arc::new(Mutex::new(None))
            }
        };

    // ── Shared state ─────────────────────────────────────────────────────────
    let dj_state = state::new_shared(startup_cfg.midi.smoothing_ms);

    // ── Broadcast channels ───────────────────────────────────────────────────
    let (device_tx, device_rx) =
        broadcast::channel::<crate::prolink::discovery::DeviceEvent>(64);
    let (beat_tx, beat_rx1) = broadcast::channel::<BeatEvent>(256);
    let (status_tx, status_rx1) = broadcast::channel::<StatusEvent>(256);
    let (vcdjready_tx, _vcdjready_rx) = broadcast::channel::<VirtualCdjReady>(4);

    let beat_rx2 = beat_tx.subscribe();
    let beat_rx3 = beat_tx.subscribe();
    let status_rx2 = status_tx.subscribe();

    let device_table = crate::prolink::discovery::DeviceTable::default();

    // ── Spawn tasks ───────────────────────────────────────────────────────────
    //
    // Which tasks run depends on the configured source:
    //   ProLink — Pro DJ Link tasks only (hardware CDJs on Ethernet)
    //   Link    — Ableton Link engine only (rekordbox Performance / USB)
    //   Auto    — both; Link fills in when no Pro DJ Link master is present

    let use_prolink = startup_cfg.source != config::Source::Link;
    let use_link    = startup_cfg.source != config::Source::ProLink;

    // 1. Device discovery (Pro DJ Link)
    if use_prolink {
        let disc_table = Arc::clone(&device_table);
        let disc_tx = device_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::prolink::discovery::run(bind_ip, disc_table, disc_tx).await {
                tracing::error!("Discovery task error: {e}");
            }
        });

        // 2. Virtual CDJ
        let vcdj_name = startup_cfg.device_name.clone();
        let vcdj_ready_tx = vcdjready_tx.clone();
        let vcdj_dev_num = startup_cfg.device_number;
        let vcdj_table = Arc::clone(&device_table);
        tokio::spawn(async move {
            if let Err(e) = crate::prolink::virtual_cdj::run(
                bind_ip,
                bcast_ip,
                mac,
                vcdj_dev_num,
                &vcdj_name,
                vcdj_table,
                vcdj_ready_tx,
            )
            .await
            {
                tracing::error!("Virtual CDJ task error: {e}");
            }
        });

        // 3. Beat listener
        let beat_tx2 = beat_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::prolink::beat_listener::run(bind_ip, beat_tx2).await {
                tracing::error!("Beat listener error: {e}");
            }
        });

        // 4. Status listener (start after the virtual CDJ claims its final number)
        let mut vcdj_ready_rx = vcdjready_tx.subscribe();
        tokio::spawn(async move {
            let ready = match vcdj_ready_rx.recv().await {
                Ok(ready) => ready,
                Err(e) => {
                    tracing::error!("Status listener could not receive virtual CDJ ready event: {e}");
                    return;
                }
            };

            if let Err(e) = crate::prolink::status_listener::run(
                bind_ip,
                ready.device_number,
                status_tx,
            )
            .await
            {
                tracing::error!("Status listener error: {e}");
            }
        });
    }

    // 5. Ableton Link engine
    if use_link {
        let link_cfg = startup_cfg.link.clone();
        let link_state = Arc::clone(&dj_state);
        let link_beat_tx = beat_tx.clone();
        tokio::spawn(async move {
            crate::link::run(link_cfg, link_state, link_beat_tx).await;
        });
        tracing::info!("Ableton Link engine scheduled");
    }

    // 6. State appliers (always run — process events from whichever source fires)
    let (track_change_tx, track_change_rx) = mpsc::channel::<crate::state::TrackChange>(64);
    tokio::spawn(beat_applier(Arc::clone(&dj_state), Arc::clone(&cfg), beat_rx1));
    tokio::spawn(status_applier(
        Arc::clone(&dj_state),
        Arc::clone(&cfg),
        status_rx1,
        track_change_tx,
    ));

    // 7. Device event logger
    tokio::spawn(device_logger(Arc::clone(&dj_state), device_rx));

    // 8. Track metadata fetcher (dbserver TCP queries)
    if use_prolink {
        let meta_table = Arc::clone(&device_table);
        let meta_state = Arc::clone(&dj_state);
        let meta_dev_num = startup_cfg.device_number;
        tokio::spawn(async move {
            crate::prolink::metadata::run(meta_dev_num, meta_table, meta_state, track_change_rx)
                .await;
        });
    }

    // 9. MIDI clock
    let midi_activity: Arc<Mutex<crate::tui::state::MidiActivity>> =
        Arc::new(Mutex::new(crate::tui::state::MidiActivity::default()));
    let clock_conn = Arc::clone(&midi_conn);
    let clock_state = Arc::clone(&dj_state);
    let clock_cfg = Arc::clone(&cfg);
    let clock_activity = Arc::clone(&midi_activity);
    tokio::spawn(async move {
        crate::midi::clock::run(clock_conn, clock_state, beat_rx2, clock_cfg, clock_activity).await;
    });

    // 10. MIDI CC/Note mapper
    let mapper_conn = Arc::clone(&midi_conn);
    let mapper_state = Arc::clone(&dj_state);
    let mapper_cfg = Arc::clone(&cfg);
    let mapper_activity = Arc::clone(&midi_activity);
    tokio::spawn(async move {
        crate::midi::mapper::run(mapper_conn, mapper_state, beat_rx3, status_rx2, mapper_cfg, mapper_activity)
            .await;
    });

    // 11. MIDI Timecode (MTC)
    let mtc_conn = Arc::clone(&midi_conn);
    let mtc_state = Arc::clone(&dj_state);
    let mtc_cfg = Arc::clone(&cfg);
    let mtc_activity = Arc::clone(&midi_activity);
    tokio::spawn(async move {
        crate::midi::timecode::run(mtc_conn, mtc_state, mtc_cfg, mtc_activity).await;
    });

    // ── TUI or headless ──────────────────────────────────────────────────────
    if use_tui {
        tui::run(dj_state, device_table, cfg, midi_conn, log_buf, midi_activity).await
    } else {
        headless_loop(dj_state, midi_conn).await
    }
}
