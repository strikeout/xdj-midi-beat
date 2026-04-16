use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio::time::{interval, Duration};
use anyhow::Result;
use std::net::SocketAddr;
use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::{StreamExt, SinkExt};

const LOG_ENTRIES: usize = 100;

#[derive(Debug, Clone)]
struct DeckState {
    device_number: u8,
    device_type: u8,
    is_master: bool,
    is_playing: bool,
    is_sync: bool,
    is_on_air: bool,
    bpm: f64,
    pitch: f64,
    beat: u8,
    phrase_16: u8,
    name: String,
    ip: Option<[u8; 4]>,
    last_seen: std::time::Instant,
}

impl Default for DeckState {
    fn default() -> Self {
        Self {
            device_number: 0,
            device_type: 0,
            is_master: false,
            is_playing: false,
            is_sync: false,
            is_on_air: false,
            bpm: 0.0,
            pitch: 0.0,
            beat: 0,
            phrase_16: 0,
            name: String::new(),
            ip: None,
            last_seen: std::time::Instant::now(),
        }
    }
}

struct LogEntry {
    message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MidiConfig {
    output: String,
    clock_enabled: bool,
    smoothing_ms: u64,
    latency_ms: i64,
    note_channel: u8,
    beat_note: u8,
    downbeat_note: u8,
    cc_channel: u8,
    bpm_coarse: u8,
    bpm_fine: u8,
    pitch_cc: u8,
    bar_phase_cc: u8,
    beat_phase_cc: u8,
    playing_cc: u8,
}

impl Default for MidiConfig {
    fn default() -> Self {
        Self {
            output: "auto".to_string(),
            clock_enabled: true,
            smoothing_ms: 30,
            latency_ms: 0,
            note_channel: 10,
            beat_note: 36,
            downbeat_note: 37,
            cc_channel: 0,
            bpm_coarse: 1,
            bpm_fine: 33,
            pitch_cc: 2,
            bar_phase_cc: 3,
            beat_phase_cc: 4,
            playing_cc: 5,
        }
    }
}

struct EmulatorState {
    virtual_device_num: u8,
    decks: Vec<DeckState>,
    tick: u64,
    master_idx: usize,
    logs: Vec<LogEntry>,
    midi_config: MidiConfig,
    midi_clock_count: u64,
    midi_note_count: u64,
    midi_cc_count: u64,
    last_note: u8,
    last_note_ch: u8,
    last_cc: u8,
    last_cc_val: u8,
    last_cc_ch: u8,
}

impl EmulatorState {
    fn new() -> Self {
        let mut decks = Vec::with_capacity(16);
        for _ in 0..16 {
            decks.push(DeckState::default());
        }

        let mut state = Self {
            virtual_device_num: 5,
            decks,
            tick: 0,
            master_idx: 0,
            logs: Vec::with_capacity(LOG_ENTRIES),
            midi_config: MidiConfig::default(),
            midi_clock_count: 0,
            midi_note_count: 0,
            midi_cc_count: 0,
            last_note: 0,
            last_note_ch: 0,
            last_cc: 0,
            last_cc_val: 0,
            last_cc_ch: 0,
        };

        state.add_log(0, "INFO", "xdj-clock ESP32 v1.1.7 starting");
        state.add_log(0, "INFO", "WiFi AP: xdj-midi-setup (pass: xdjclock123)");
        state.add_log(0, "INFO", "Ethernet: 10.10.10.50/24");
        state.add_log(0, "INFO", "MIDI: GPIO1(IN) GPIO3(OUT) @ 31250");
        state.add_log(0, "INFO", "Pro DJ Link virtual CDJ #5");
        state.add_log(0, "INFO", "Listening on UDP:50000,50001,50002");
        state.add_log(0, "INFO", "Proactive participation enabled (keep-alive + status templates)");
        state.add_log(0, "INFO", "Ready for Pro DJ Link packets...");
        state
    }

    fn add_log(&mut self, _level: u8, prefix: &str, msg: impl Into<String>) {
        let full_msg = format!("{} {}", prefix, msg.into());
        let entry = LogEntry {
            message: full_msg.clone(),
        };
        if self.logs.len() >= LOG_ENTRIES {
            self.logs.remove(0);
        }
        self.logs.push(entry);
        println!("  {}", full_msg);
    }

    fn refresh_master(&mut self) {
        let prev_master = self.master_idx;
        
        // 1. Priority: Explicit master flag from a deck
        let explicit_master = self.decks.iter().position(|d| d.is_master && d.device_number != 0);
        
        if let Some(idx) = explicit_master {
            if self.master_idx != idx {
                let deck = &self.decks[idx];
                self.add_log(0, "INFO", format!("Master handoff: #{} ({}) is now MASTER", deck.device_number, deck.name));
            }
            self.master_idx = idx;
            return;
        }

        // 2. Fallback: First playing deck (if no explicit master)
        let playing_deck = self.decks.iter().position(|d| d.is_playing && d.device_number != 0);
        if let Some(idx) = playing_deck {
            if self.master_idx != idx {
                let deck = &self.decks[idx];
                self.add_log(0, "INFO", format!("Master fallback: #{} ({}) is PLAYING", deck.device_number, deck.name));
            }
            self.master_idx = idx;
        } else {
            // 3. Last resort: Keep current master if it still exists, otherwise reset to 0
            if self.master_idx >= self.decks.len() || self.decks[self.master_idx].device_number == 0 {
                if prev_master != 0 {
                    self.add_log(0, "WARN", "Master lost: no active or playing decks");
                }
                self.master_idx = 0;
            }
        }
    }

    fn handle_packet(&mut self, addr: SocketAddr, port: u16, data: &[u8]) {
        if data.len() < 11 {
            return;
        }

        if &data[..10] != xdj_clock_host::prolink::MAGIC {
            return;
        }

        let pkt_type = data[10];

        match port {
            xdj_clock_host::prolink::PORT_DISCOVERY => self.handle_discovery_packet(addr, pkt_type, data),
            xdj_clock_host::prolink::PORT_BEAT => self.handle_beat_packet(addr, pkt_type, data),
            xdj_clock_host::prolink::PORT_STATUS => self.handle_status_packet(addr, pkt_type, data),
            _ => {}
        }

        self.refresh_master();
    }

    fn handle_discovery_packet(&mut self, addr: SocketAddr, pkt_type: u8, data: &[u8]) {
        use xdj_clock_host::prolink::packets;
        match pkt_type {
            xdj_clock_host::prolink::PKT_KEEPALIVE => {
                if let Some(ka) = packets::parse_keepalive(data) {
                    let type_name = if ka.device_number == 33 {
                        "Mixer"
                    } else if ka.device_number >= 1 && ka.device_number <= 4 {
                        "CDJ"
                    } else {
                        match ka.device_type {
                            1 => "Mixer",
                            2 | 4 => "CDJ",
                            3 => "XDJ",
                            _ => "Unknown",
                        }
                    };

                    if let Some(idx) = self.find_deck(ka.device_number) {
                        self.decks[idx].ip = Some(ka.ip);
                        self.decks[idx].last_seen = std::time::Instant::now();
                        if self.decks[idx].name.is_empty() {
                            self.decks[idx].device_type = ka.device_type;
                            self.decks[idx].name = ka.name.clone();
                            self.add_log(0, "INFO", format!("Identified: {} #{} ({})", ka.name, ka.device_number, type_name));
                        }
                    } else if let Some(slot) = self.find_empty_slot() {
                        self.decks[slot].device_number = ka.device_number;
                        self.decks[slot].device_type = ka.device_type;
                        self.decks[slot].name = ka.name.clone();
                        self.decks[slot].ip = Some(ka.ip);
                        self.decks[slot].last_seen = std::time::Instant::now();
                        self.add_log(0, "INFO", format!("Discovered: {} #{} ({})", ka.name, ka.device_number, type_name));
                    }
                }
            }
            xdj_clock_host::prolink::PKT_ANNOUNCE => {
                if data.len() >= 12 {
                    let device_num = data[11];
                    self.add_log(2, "DEBUG", format!("Announce #{}", device_num));
                }
            }
            0x00 | 0x02 | 0x04 => {
                if data.len() >= 12 {
                    let device_num = data[11];
                    self.add_log(2, "DEBUG", format!("Claim {}: device #{}", pkt_type, device_num));
                }
            }
            0x08 => {
                self.add_log(1, "WARN", format!("Conflict: {}", addr));
            }
            _ => {}
        }
    }

    fn handle_beat_packet(&mut self, _addr: SocketAddr, pkt_type: u8, data: &[u8]) {
        use xdj_clock_host::prolink::packets;
        match pkt_type {
            xdj_clock_host::prolink::PKT_BEAT => {
                if let Some(beat) = packets::parse_beat(data) {
                    if let Some(idx) = self.find_deck(beat.device_number) {
                        self.decks[idx].bpm = beat.effective_bpm;
                        self.decks[idx].pitch = beat.pitch_pct;
                        self.decks[idx].beat = if beat.beat_in_bar == 0 { 4 } else { beat.beat_in_bar };
                        self.decks[idx].last_seen = std::time::Instant::now();

                        let old_beat = self.decks[idx].beat;
                        if old_beat == 4 && self.decks[idx].beat == 1 {
                            self.decks[idx].phrase_16 = (self.decks[idx].phrase_16 % 16) + 1;
                        }

                        // Beat packets are authoritative for "playing" state
                        self.decks[idx].is_playing = true;
                    }

                    if self.decks.get(self.master_idx).map(|d| d.device_number) == Some(beat.device_number) {
                        self.midi_note_count += 1;
                        self.midi_cc_count += 1;
                        self.last_note = self.midi_config.beat_note;
                        self.last_note_ch = self.midi_config.note_channel;
                        self.last_cc = self.midi_config.beat_phase_cc;
                        self.last_cc_val = (beat.beat_in_bar * 31) as u8;
                        self.last_cc_ch = self.midi_config.cc_channel;
                    }

                    self.add_log(2, "DEBUG", format!("Beat #{} bpm={:.2} beat={}/4", beat.device_number, beat.effective_bpm, beat.beat_in_bar));
                }
            }
            xdj_clock_host::prolink::PKT_ABS_POSITION => {
                if let Some(abs) = packets::parse_abs_position(data) {
                    if let Some(idx) = self.find_deck(abs.device_number) {
                        self.decks[idx].bpm = abs.effective_bpm;
                        self.decks[idx].pitch = abs.pitch_pct;
                        self.decks[idx].last_seen = std::time::Instant::now();
                    }
                    if self.decks.get(self.master_idx).map(|d| d.device_number) == Some(abs.device_number) {
                        self.midi_cc_count += 1;
                        self.last_cc = self.midi_config.bar_phase_cc;
                        self.last_cc_val = 64;
                        self.last_cc_ch = self.midi_config.cc_channel;
                    }
                    self.add_log(2, "DEBUG", format!("AbsPosition #{} pos={}ms bpm={:.2}", abs.device_number, abs.playhead_ms, abs.effective_bpm));
                }
            }
            _ => {}
        }
    }

    fn handle_status_packet(&mut self, _addr: SocketAddr, pkt_type: u8, data: &[u8]) {
        use xdj_clock_host::prolink::packets;
        match pkt_type {
            xdj_clock_host::prolink::PKT_CDJ_STATUS => {
                if let Some(s) = packets::parse_cdj_status(data) {
                    if let Some(idx) = self.find_deck(s.device_number) {
                        self.decks[idx].is_playing = s.play_state.is_playing() || s.is_playing_flag;
                        self.decks[idx].is_sync = s.is_sync;
                        self.decks[idx].is_on_air = s.is_on_air;
                        self.decks[idx].pitch = s.pitch_pct;
                        self.decks[idx].last_seen = std::time::Instant::now();
                        
                        if s.is_master {
                            // Clear master flag from all other decks if this one claims it
                            for i in 0..self.decks.len() {
                                if i != idx {
                                    self.decks[i].is_master = false;
                                }
                            }
                        }
                        
                        self.decks[idx].is_master = s.is_master;
                        self.decks[idx].bpm = s.effective_bpm;
                        self.decks[idx].beat = s.beat_in_bar;
                    }

                    let playing_str = if s.play_state.is_playing() || s.is_playing_flag { "playing" } else { "stopped" };
                    let master_str = if s.is_master { " MASTER" } else { "" };
                    self.add_log(0, "INFO", format!("Status #{} {} bpm={:.2}{} (beat={})", s.device_number, playing_str, s.effective_bpm, master_str, s.beat_in_bar));
                }
            }
            xdj_clock_host::prolink::PKT_MIXER_STATUS => {
                if let Some(s) = packets::parse_mixer_status(data) {
                    if let Some(idx) = self.find_deck(s.device_number) {
                        self.decks[idx].last_seen = std::time::Instant::now();
                        if s.is_master {
                            for i in 0..self.decks.len() {
                                if i != idx {
                                    self.decks[i].is_master = false;
                                }
                            }
                        }
                        self.decks[idx].is_master = s.is_master;
                        if let Some(bpm) = s.track_bpm {
                            self.decks[idx].bpm = bpm;
                        }
                    }
                    self.add_log(0, "DEBUG", format!("Mixer #{} is {} (bpm={:?})", s.device_number, if s.is_master { "master" } else { "slave" }, s.track_bpm));
                }
            }
            _ => {}
        }
    }

    fn find_empty_slot(&self) -> Option<usize> {
        for (i, d) in self.decks.iter().enumerate() {
            if d.device_number == 0 { return Some(i); }
        }
        None
    }

    fn find_deck(&self, device_num: u8) -> Option<usize> {
        self.decks.iter().position(|d| d.device_number == device_num)
    }

    fn tick(&mut self) {
        self.tick += 1;
        
        let now = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(2);

        // Expire "playing" state and master flag if no packets for a while
        for deck in self.decks.iter_mut().filter(|d| d.device_number != 0) {
            if now.duration_since(deck.last_seen) > timeout {
                if deck.is_playing {
                    deck.is_playing = false;
                }
                if deck.is_master {
                    deck.is_master = false;
                }
            }
        }

        self.refresh_master();

        let master_idx = self.master_idx;
        let master_bpm = if master_idx < self.decks.len() { self.decks[master_idx].bpm } else { 0.0 };
        let master_playing = if master_idx < self.decks.len() { self.decks[master_idx].is_playing } else { false };
        
        if master_bpm > 0.0 && master_playing {
            let clocks_to_add = ((24.0 * master_bpm) / 600.0).round() as u64;
            if clocks_to_add > 0 {
                self.midi_clock_count += clocks_to_add;
            } else if self.tick % 5 == 0 {
                self.midi_clock_count += 1;
            }
        }

        if self.tick % 500 == 0 {
            self.add_log(0, "INFO", "MIDI clock: 24PPQ running");
        }
    }

    fn visible_decks(&self) -> Vec<&DeckState> {
        self.decks.iter()
            .filter(|d| d.device_number != 0 && d.device_number != self.virtual_device_num)
            .collect()
    }

    fn device_type_name(t: u8) -> &'static str {
        match t {
            1 => "Mixer",
            2 => "CDJ",
            3 => "XDJ",
            _ => "Unknown",
        }
    }

    fn get_status_json(&self) -> String {
        let master_idx = self.decks.iter().position(|d| d.is_master)
            .or_else(|| self.decks.iter().position(|d| d.is_playing && d.device_number != 0))
            .unwrap_or(self.master_idx);
        let master = if master_idx < self.decks.len() { &self.decks[master_idx] } else { &self.decks[0] };
        let decks = self.visible_decks();
        let cfg = &self.midi_config;

        let mut json = format!(
            "{{\"bpm\":{:.2},\"beat\":{},\"phrase16\":{},\"playing\":{},\"master\":{},\"pitch\":{:.2},\"decks\":[",
            master.bpm, master.beat, master.phrase_16, master.is_playing as u8, master.device_number, master.pitch
        );

        for (i, deck) in decks.iter().enumerate() {
            if i > 0 { json.push(','); }
            let safe_name = deck.name.replace('\0', " ");
            json.push_str(&format!(
                "{{\"num\":{},\"type\":{},\"typename\":\"{}\",\"name\":\"{}\",\"playing\":{},\"master\":{},\"sync\":{},\"onair\":{},\"bpm\":{:.2},\"beat\":{}}}",
                deck.device_number, deck.device_type, Self::device_type_name(deck.device_type),
                safe_name, deck.is_playing as u8,
                deck.is_master as u8, deck.is_sync as u8,
                deck.is_on_air as u8, deck.bpm, deck.beat
            ));
        }

        json.push_str("],\"midi_counts\":[");
        json.push_str(&format!("{},{},{}", self.midi_clock_count, self.midi_note_count, self.midi_cc_count));
        json.push_str("],\"midi_last\":{");
        json.push_str(&format!(
            "\"note\":{},\"noteCh\":{},\"cc\":{},\"ccVal\":{},\"ccCh\":{}",
            self.last_note, self.last_note_ch, self.last_cc, self.last_cc_val, self.last_cc_ch
        ));
        json.push_str("},\"midi\":{");
        json.push_str(&format!(
            "\"output\":\"{}\",\"clock\":{},\"smoothing\":{},\"latency\":{},\"noteCh\":{},\"beatNote\":{},\"downbeatNote\":{},\"ccCh\":{},\"bpmCoarse\":{},\"bpmFine\":{},\"pitchCc\":{},\"barPhaseCc\":{},\"beatPhaseCc\":{},\"playingCc\":{}",
            cfg.output, cfg.clock_enabled as u8, cfg.smoothing_ms, cfg.latency_ms,
            cfg.note_channel, cfg.beat_note, cfg.downbeat_note,
            cfg.cc_channel, cfg.bpm_coarse, cfg.bpm_fine, cfg.pitch_cc,
            cfg.bar_phase_cc, cfg.beat_phase_cc, cfg.playing_cc
        ));
        json.push_str("},\"logs\":[");

        let logs: Vec<String> = self.logs.iter()
            .map(|l| format!("\"{}\"", l.message.replace('"', "\\\"")))
            .collect();
        json.push_str(&logs.join(","));
        json.push_str("]}");
        json
    }
}

pub async fn run_emulator() -> Result<()> {
    println!("╔══════════════════════════════════════════╗");
    println!("║     xdj-clock ESP32 Emulator v1.1.2     ║");
    println!("╚══════════════════════════════════════════╝");

    fn create_reuse_socket(port: u16, broadcast: bool) -> Result<tokio::net::UdpSocket> {
        let std_sock = xdj_clock_host::prolink::create_reuse_socket(port)?;
        if broadcast {
            std_sock.set_broadcast(true)?;
        }
        let tokio_sock = tokio::net::UdpSocket::from_std(std_sock)?;
        Ok(tokio_sock)
    }

    let state = Arc::new(RwLock::new(EmulatorState::new()));
    let (tx, _rx) = broadcast::channel::<String>(16);

    let state_net = Arc::clone(&state);
    let sock_50000 = create_reuse_socket(50000, true).unwrap();
    let sock_50000 = Arc::new(sock_50000);
    let sock_50000_recv = Arc::clone(&sock_50000);
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            if let Ok((len, addr)) = sock_50000_recv.recv_from(&mut buf).await {
                state_net.write().await.handle_packet(addr, 50000, &buf[..len]);
            }
        }
    });

    let state_net2 = Arc::clone(&state);
    tokio::spawn(async move {
        let sock = create_reuse_socket(50001, false).unwrap();
        let mut buf = [0u8; 4096];
        loop {
            if let Ok((len, addr)) = sock.recv_from(&mut buf).await {
                state_net2.write().await.handle_packet(addr, 50001, &buf[..len]);
            }
        }
    });

    let state_net3 = Arc::clone(&state);
    tokio::spawn(async move {
        let sock = create_reuse_socket(50002, false).unwrap();
        let mut buf = [0u8; 4096];
        loop {
            if let Ok((len, addr)) = sock.recv_from(&mut buf).await {
                state_net3.write().await.handle_packet(addr, 50002, &buf[..len]);
            }
        }
    });

    // Participation loop: keep-alive broadcasts and status unicasts
    let state_part = Arc::clone(&state);
    let sock_part = Arc::clone(&sock_50000);
    tokio::spawn(async move {
        use xdj_clock_host::prolink::builder;
        let mut interval = interval(Duration::from_millis(1500));
        let mac = [0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x02];
        let name = builder::pad_name("xdj-clock-emu");
        
        loop {
            interval.tick().await;
            let s = state_part.read().await;
            let dev_num = s.virtual_device_num;
            
            // 1. Broadcast keep-alive
            // We use 10.10.10.50 as our IP in the packet for the emulator
            let my_ip = [10, 10, 10, 50];
            let ka_pkt = builder::build_keepalive(&name, dev_num, &mac, &my_ip, 1);
            if let Err(e) = sock_part.send_to(&ka_pkt, "255.255.255.255:50000").await {
                eprintln!("Failed to send keep-alive: {}", e);
            }
            
            // 2. Unicast status template to all discovered decks on port 50002
            let status_pkt = builder::build_status_packet(&name, dev_num);
            for deck in s.decks.iter().filter(|d| d.device_number != 0 && d.ip.is_some()) {
                if let Some(ip) = deck.ip {
                    let addr = SocketAddr::from((ip, 50002));
                    if let Err(e) = sock_part.send_to(&status_pkt, addr).await {
                        eprintln!("Failed to send status template to {}: {}", addr, e);
                    }
                }
            }
        }
    });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    println!("HTTP server listening on http://0.0.0.0:8080");

    let state_clone = Arc::clone(&state);
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(50));
        loop {
            ticker.tick().await;
            let mut s = state_clone.write().await;
            s.tick();
            if s.tick % 2 == 0 {
                let json = s.get_status_json();
                let _ = tx_clone.send(json);
            }
        }
    });

    loop {
        if let Ok((stream, _addr)) = listener.accept().await {
            let state = Arc::clone(&state);
            let tx = tx.clone();
            let mut rx = tx.subscribe();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                
                let mut buf = [0u8; 8192];
                let mut stream = stream;
                let n = stream.peek(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                
                if request.contains("Upgrade: websocket") {
                    if let Ok(ws_stream) = tokio_tungstenite::accept_async(stream).await {
                        let (mut ws_sender, mut _ws_receiver) = ws_stream.split();
                        while let Ok(msg) = rx.recv().await {
                            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                } else if let Ok(n) = stream.read(&mut buf).await {
                    if n > 0 {
                        let request = String::from_utf8_lossy(&buf[..n]);
                        if request.contains("/api/status") {
                            let status = state.read().await.get_status_json();
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\r\n{}",
                                status.len(),
                                status
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        } else if request.contains("/api/set-midi") {
                            let body_parts: Vec<&str> = request.split("\r\n\r\n").collect();
                            if body_parts.len() > 1 {
                                let body = body_parts[1];
                                if let Ok(new_cfg) = serde_json::from_str::<MidiConfig>(body) {
                                    state.write().await.midi_config = new_cfg;
                                    println!("MIDI config updated successfully");
                                }
                            }
                            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\n\r\n{}";
                            let _ = stream.write_all(response.as_bytes()).await;
                        } else if request.contains("GET / ") || request.contains("GET /index") {
                            let html = get_dashboard_html();
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}", html.len(), html);
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                    }
                }
            });
        }
    }
}

fn get_dashboard_html() -> &'static str {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>xdj-clock ESP32</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;background:#0a0a0f;color:#e0e0e0;min-height:100vh;display:flex;flex-direction:column}
header{background:#1a1a2e;padding:10px 20px;border-bottom:1px solid #2a2a4a;display:flex;justify-content:space-between;align-items:center}
h1{font-size:15px;font-weight:600;color:#fff}
h1 span{color:#666;font-weight:400}
.header-meta{display:flex;align-items:center;gap:12px}
.version-tag{font-size:10px;color:#555;font-family:monospace}
.emulator-badge{background:#ff6b35;color:#000;padding:3px 8px;border-radius:4px;font-size:10px;font-weight:700}
main{flex:1;padding:12px;display:grid;grid-template-columns:260px 1fr;grid-template-rows:auto auto auto;gap:12px;max-width:1600px;margin:0 auto;width:100%}
.panel{background:#12121c;border-radius:10px;padding:14px;border:1px solid #1e1e32;overflow:hidden}
.panel-header{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px;padding-bottom:8px;border-bottom:1px solid #1e1e32}
.panel h2{font-size:10px;text-transform:uppercase;letter-spacing:1.5px;color:#666}
.panel-badge{background:#00ff88;color:#000;padding:2px 6px;border-radius:3px;font-size:9px;font-weight:700}
.deck-section{grid-column:1;grid-row:1/3;display:flex;flex-direction:column;min-height:0}
.master-section{grid-column:2;grid-row:1}
.midi-out-status{grid-column:2;grid-row:2}
.log-section{grid-column:1/-1;grid-row:3;height:160px;display:flex;flex-direction:column}
.midi-section{grid-column:1/-1;grid-row:4;background:#12121c;border-radius:10px;border:1px solid #1e1e32;padding:14px}
.bpm-display{font-size:38px;font-weight:700;color:#00ff88;text-align:center;padding:8px 0 4px}
.beat-display{text-align:center;font-size:12px;color:#666;margin-bottom:8px}
.meter-row{display:flex;flex-direction:column;align-items:center;gap:4px}
.meter{width:100%;display:flex;gap:2px;justify-content:center}
.meter span{width:9px;height:16px;background:#1e1e32;border-radius:2px}
.meter span.active{background:#00ff88;box-shadow:0 0 5px #00ff88}
.meter16{width:100%;height:6px;background:#1a1a2e;border-radius:2px;margin-top:3px;overflow:hidden}
.meter16-fill{height:100%;background:linear-gradient(90deg,#00ff88,#00cc6a);transition:width 0.1s}
.phrase-label{font-size:9px;color:#555;text-align:center;margin-top:2px}
.info-table{width:100%;font-size:11px;margin-top:8px}
.info-row{display:flex;justify-content:space-between;padding:3px 0;border-bottom:1px solid #1a1a2e}
.info-row:last-child{border-bottom:none}
.label{color:#666}
.value{color:#fff;font-weight:500}
.deck-list{flex:1;overflow-y:auto;min-height:0;padding-right:4px}
.deck-card{display:flex;align-items:center;gap:8px;padding:8px;background:#1a1a2e;border-radius:8px;margin-bottom:6px}
.deck-card.master{border:1px solid #00ff88;background:rgba(0,255,136,0.05)}
.deck-card.playing{background:#1a2e1a}
.deck-icon{width:32px;height:32px;background:#2a2a3e;border-radius:5px;display:flex;align-items:center;justify-content:center;font-size:12px;font-weight:700;color:#fff;flex-shrink:0}
.deck-card.master .deck-icon{background:#00ff88;color:#000}
.deck-info{flex:1;min-width:0}
.deck-name{font-size:12px;font-weight:600;color:#fff;margin-bottom:1px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.deck-meta{font-size:10px;color:#666}
.deck-status{display:flex;gap:4px;flex-wrap:wrap}
.status-badge{padding:1px 5px;border-radius:3px;font-size:8px;font-weight:600;text-transform:uppercase}
.badge-master{background:#00ff88;color:#000}
.badge-sync{background:#444;color:#fff}
.badge-onair{background:#ff4444;color:#fff}
.badge-playing{background:#00ff88;color:#000}
.deck-bpm{font-size:13px;font-weight:600;color:#00ff88;text-align:right;min-width:50px}
.midi-out-status{background:#12121c;border-radius:10px;padding:14px;border:1px solid #1e1e32;display:flex;flex-direction:column;justify-content:center}
.midi-types{display:flex;justify-content:space-around;margin-top:16px;gap:8px}
.midi-type{display:flex;flex-direction:column;align-items:center;gap:6px}
.midi-type-label{font-size:10px;color:#666;font-weight:600;letter-spacing:1px}
.midi-indicator{width:40px;height:24px;background:#0d0d15;border-radius:6px;display:flex;justify-content:center;align-items:center;border:1px solid #1e1e32;overflow:hidden;position:relative}
.midi-indicator-label{position:absolute;bottom:1px;font-size:7px;color:#444;width:100%;text-align:center}
.anim-clock .dot{width:8px;height:8px;border-radius:50%;background:#1e1e32;transition:all 0.05s}
.anim-clock.active .dot{background:#ffaa00;box-shadow:0 0 8px #ffaa00;transform:scale(1.2)}
.anim-note{display:flex;gap:2px;padding:0 4px}
.anim-note .bar{width:8px;height:12px;background:#1e1e32;border-radius:2px;transition:all 0.05s}
.anim-note.active .bar{background:#00ff88;box-shadow:0 0 8px #00ff88}
.anim-cc{display:flex;align-items:flex-end;height:16px;gap:2px}
.anim-cc .bar{width:6px;height:4px;background:#1e1e32;border-radius:1px;transition:all 0.05s}
.anim-cc.active .bar{background:#4a9eff;box-shadow:0 0 8px #4a9eff}
.midi-stats{display:flex;justify-content:space-between;font-size:11px;color:#666;margin-top:16px;padding-top:12px;border-top:1px solid #1a1a2e}
.midi-detail{font-size:9px;color:#00ff88;text-align:center;margin-top:4px;min-height:12px}
.log-container{flex:1;overflow-y:auto;font-family:'SF Mono',Monaco,'Courier New',monospace;font-size:10px;background:#0a0a0f;border-radius:6px;padding:8px}
.log-entry{padding:2px 0;border-bottom:1px solid #1a1a2e;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.log-entry:last-child{border-bottom:none}
.log-time{color:#444;margin-right:5px}
.log-info{color:#888}
.log-debug{color:#555}
.log-warn{color:#ffaa00}
.log-error{color:#ff4444}
.midi-header{display:flex;justify-content:space-between;align-items:center;margin-bottom:12px}
.midi-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px}
.midi-group{background:#1a1a2e;border-radius:6px;padding:10px}
.midi-group h3{font-size:9px;text-transform:uppercase;letter-spacing:1px;color:#666;margin-bottom:8px}
.midi-row{display:flex;justify-content:space-between;align-items:center;padding:3px 0;font-size:11px}
.midi-row label{color:#888}
.midi-row input{background:#0a0a0f;border:1px solid #2a2a4a;border-radius:4px;color:#fff;padding:3px 6px;font-size:11px;width:60px;text-align:right}
.midi-row input[type="checkbox"]{width:auto}
.midi-row select{background:#0a0a0f;border:1px solid #2a2a4a;border-radius:4px;color:#fff;padding:3px 6px;font-size:11px;width:80px}
footer{background:#1a1a2e;padding:8px 20px;text-align:center;font-size:10px;color:#444;border-top:1px solid #2a2a4a}
@media(max-width:768px){
  main{grid-template-columns:1fr;grid-template-rows:auto auto auto auto auto}
  .deck-section,.master-section,.midi-out-status,.log-section,.midi-section{grid-column:1}
  .deck-section{grid-row:1;height:250px}
  .master-section{grid-row:2}
  .midi-out-status{grid-row:3}
  .log-section{grid-row:4;height:150px}
  .midi-section{grid-row:5}
}
</style>
</head>
<body>
<header>
<h1>xdj-clock <span>ESP32</span></h1>
<div class="header-meta">
<span class="version-tag">v1.1.2</span>
<span class="emulator-badge">EMULATOR</span>
</div>
</header>
<main>
<div class="panel deck-section"><div class="panel-header"><h2>Discovered Decks</h2><span class="panel-badge" id="deck-count">0</span></div><div class="deck-list" id="deck-list"></div></div>
<div class="panel master-section"><div class="panel-header"><h2>Tempo Master</h2></div><div class="bpm-display" id="bpm">---.-</div><div class="beat-display">BPM</div><div class="meter-row"><div class="meter" id="meter"><span></span><span></span><span></span><span></span></div><div class="phrase-label">Phrase <span id="phrase16">-</span>/16</div><div class="meter16"><div class="meter16-fill" id="meter16-fill" style="width:0%"></div></div></div><div class="info-table"><div class="info-row"><span class="label">Beat</span><span class="value" id="beat">-/4</span></div><div class="info-row"><span class="label">Status</span><span class="value" id="playing" style="color:#00ff88">STOP</span></div><div class="info-row"><span class="label">Master</span><span class="value" id="master">--</span></div><div class="info-row"><span class="label">Pitch</span><span class="value" id="pitch">+0.00%</span></div></div></div>
<div class="panel midi-out-status"><div class="panel-header"><h2>MIDI Out Status</h2><span id="active-port" style="font-size:9px;color:#666">auto</span></div><div class="midi-types">
<div class="midi-type"><span class="midi-type-label">CLOCK</span><div class="midi-indicator anim-clock" id="animClock"><div class="dot"></div></div><div class="midi-detail" id="midiDetailClock"></div></div><div class="midi-type"><span class="midi-type-label">NOTE</span><div class="midi-indicator anim-note" id="animNote"><div class="bar"></div><div class="bar"></div><div class="bar"></div></div><div class="midi-detail" id="midiDetailNote"></div></div><div class="midi-type"><span class="midi-type-label">CC</span><div class="midi-indicator anim-cc" id="animCc"><div class="bar"></div><div class="bar"></div><div class="bar"></div><div class="bar"></div><div class="bar"></div></div><div class="midi-detail" id="midiDetailCc"></div></div></div><div class="midi-stats"><span>Activity</span><span id="midiCount">0 CLK | 0 NOT | 0 CC</span></div></div>
<div class="panel log-section"><div class="panel-header"><h2>Activity Log</h2></div><div class="log-container" id="log-container"></div></div>
<div class="midi-section"><div class="midi-header"><h2>MIDI Configuration</h2><span class="panel-badge">ESP32</span></div><div class="midi-grid">
<div class="midi-group"><h3>Output</h3><div class="midi-row"><label>Port</label><input type="text" id="midi-output"></div><div class="midi-row"><label>Clock</label><input type="checkbox" id="midi-clock"></div><div class="midi-row"><label>Smoothing</label><input type="number" id="midi-smoothing"> ms</div><div class="midi-row"><label>Latency</label><div style="display:flex;align-items:center;gap:8px;flex:1"><input type="range" id="midi-latency" min="-500" max="500" step="1" style="flex:1"><span id="latency-val" style="min-width:45px;text-align:right;font-variant-numeric:tabular-nums;font-size:10px;color:#00ff88">0 ms</span></div></div></div>
<div class="midi-group"><h3>Note Messages</h3><div class="midi-row"><label>Channel</label><select id="midi-note-ch"><option value="0">1</option><option value="9">10</option><option value="15">16</option></select></div><div class="midi-row"><label>Beat Note</label><input type="number" id="midi-beat-note"></div><div class="midi-row"><label>Downbeat</label><input type="number" id="midi-downbeat"></div></div>
<div class="midi-group"><h3>CC Messages</h3><div class="midi-row"><label>Channel</label><select id="midi-cc-ch"><option value="0" selected>1</option><option value="9">10</option></select></div><div class="midi-row"><label>BPM Coarse</label><input type="number" id="midi-bpm-coarse"></div><div class="midi-row"><label>BPM Fine</label><input type="number" id="midi-bpm-fine"></div><div class="midi-row"><label>Pitch</label><input type="number" id="midi-pitch-cc"></div><div class="midi-row"><label>Bar Phase</label><input type="number" id="midi-bar-cc"></div><div class="midi-row"><label>Beat Phase</label><input type="number" id="midi-beat-cc"></div><div class="midi-row"><label>Playing</label><input type="number" id="midi-playing-cc"></div></div>
</div></div>
</main>
<script>
let startTime=Date.now();
function ts(){return ((Date.now()-startTime)/1000).toFixed(1)+'s';}
let prevClock=0, prevNote=0, prevCc=0;
let flashClock=0, flashNote=0, flashCc=0;
let settingsLoaded=false;
let debounceTimer;
let ws;

function updateMidiAnimations(){
    if(flashClock>0){ document.getElementById('animClock').classList.add('active'); flashClock--; } else { document.getElementById('animClock').classList.remove('active'); }
    if(flashNote>0){ document.getElementById('animNote').classList.add('active'); flashNote--; } else { document.getElementById('animNote').classList.remove('active'); }
    if(flashCc>0){ 
        const bars=document.querySelectorAll('#animCc .bar');
        bars.forEach(b=>{b.style.height=(Math.random()*12+4)+'px';});
        document.getElementById('animCc').classList.add('active'); flashCc--; 
    } else { 
        document.getElementById('animCc').classList.remove('active'); 
        document.querySelectorAll('#animCc .bar').forEach(b=>{b.style.height='';});
    }
}

function handleData(data){
    document.getElementById('bpm').textContent=data.bpm.toFixed(2);
    document.getElementById('beat').textContent=data.beat+'/4';
    document.getElementById('phrase16').textContent=data.phrase16||'-';
    document.getElementById('master').textContent=data.master||'--';
    document.getElementById('pitch').textContent=(data.pitch>=0?'+':'')+data.pitch.toFixed(2)+'%';
    document.getElementById('playing').textContent=data.playing?'PLAYING':'STOP';
    document.getElementById('playing').style.color=data.playing?'#00ff88':'#888';
    let m=document.getElementById('meter').children;
    for(let i=0;i<m.length;i++)m[i].className=i<data.beat?'active':'';
    document.getElementById('meter16-fill').style.width=((data.phrase16||1)/16*100)+'%';
    
    let decks=data.decks||[];
    document.getElementById('deck-count').textContent=decks.length;
    let html='';
    decks.forEach(d=>{
        let cc='deck-card'+(d.master?' master':'')+(d.playing?' playing':'');
        let badges=(d.master?'<span class="status-badge badge-master">M</span>':'')+(d.sync?'<span class="status-badge badge-sync">SY</span>':'')+(d.onair?'<span class="status-badge badge-onair">ON</span>':'')+(d.playing?'<span class="status-badge badge-playing">▶</span>':'');
        html+=`<div class="${cc}"><div class="deck-icon">${d.type===1?'M':'D'}</div><div class="deck-info"><div class="deck-name">${d.name||d.typename+' #'+d.num}</div><div class="deck-meta">${d.typename} • Beat ${d.beat}/4</div></div><div class="deck-status">${badges}</div><div class="deck-bpm">${d.bpm.toFixed(1)}</div></div>`;
    });
    document.getElementById('deck-list').innerHTML=html||'<div style="color:#555;text-align:center;padding:20px">Waiting for packets...</div>';
    
    if(data.midi_counts){
        if(data.midi_counts[0]>prevClock) flashClock=3;
        if(data.midi_counts[1]>prevNote) flashNote=3;
        if(data.midi_counts[2]>prevCc) flashCc=3;
        prevClock=data.midi_counts[0]; prevNote=data.midi_counts[1]; prevCc=data.midi_counts[2];
        document.getElementById('midiCount').textContent=prevClock+' CLK | '+prevNote+' NOT | '+prevCc+' CC';
    }

    if(data.midi_last){
        document.getElementById('midiDetailNote').textContent=data.midi_last.note?'CH'+(data.midi_last.noteCh+1)+' N'+data.midi_last.note:'';
        document.getElementById('midiDetailCc').textContent=data.midi_last.cc?'CH'+(data.midi_last.ccCh+1)+' CC'+data.midi_last.cc+' V'+data.midi_last.ccVal:'';
        document.getElementById('midiDetailClock').textContent=data.bpm>0?'24PPQ':'';
    }
    
        if(data.midi){
        document.getElementById('active-port').textContent=data.midi.output;
    }

    if(data.midi && !settingsLoaded){
            const set=(id,v,p='value')=>{const el=document.getElementById(id);if(el)el[p]=v;};
            set('midi-output',data.midi.output); set('midi-clock',data.midi.clock,'checked'); set('midi-smoothing',data.midi.smoothing);
            set('midi-latency',data.midi.latency); set('midi-note-ch',data.midi.noteCh); set('midi-beat-note',data.midi.beatNote);
            set('midi-downbeat',data.midi.downbeatNote); set('midi-cc-ch',data.midi.ccCh); set('midi-bpm-coarse',data.midi.bpmCoarse);
            set('midi-bpm-fine',data.midi.bpmFine); set('midi-pitch-cc',data.midi.pitchCc); set('midi-bar-cc',data.midi.barPhaseCc);
            set('midi-beat-cc',data.midi.beatPhaseCc); set('midi-playing-cc',data.midi.playingCc);
            
            document.getElementById('latency-val').textContent=data.midi.latency+' ms';
            document.getElementById('active-port').textContent=data.midi.output;

            const inputs = ['midi-output', 'midi-clock', 'midi-smoothing', 'midi-latency', 'midi-note-ch', 'midi-beat-note', 'midi-downbeat', 'midi-cc-ch', 'midi-bpm-coarse', 'midi-bpm-fine', 'midi-pitch-cc', 'midi-bar-cc', 'midi-beat-cc', 'midi-playing-cc'];
            inputs.forEach(id => {
                const el = document.getElementById(id);
                const event = el.type === 'range' || el.type === 'number' || el.type === 'text' ? 'input' : 'change';
                el.addEventListener(event, (e) => {
                    if (id === 'midi-latency') document.getElementById('latency-val').textContent=e.target.value+' ms';
                    if (id === 'midi-output') document.getElementById('active-port').textContent=e.target.value;
                    clearTimeout(debounceTimer);
                    debounceTimer=setTimeout(saveMidiConfig,100);
                });
            });
            
            settingsLoaded=true;
        }

    
    let logHtml='';
    (data.logs||[]).forEach(l=>{
        let cls='log-entry'+(l.includes('DEBUG')?' log-debug':l.includes('WARN')?' log-warn':l.includes('ERROR')?' log-error':' log-info');
        logHtml+=`<div class="${cls}"><span class="log-time">${ts()}</span> ${l}</div>`;
    });
    document.getElementById('log-container').innerHTML=logHtml;
    document.getElementById('log-container').scrollTop=document.getElementById('log-container').scrollHeight;
}

function initWS(){
    ws=new WebSocket('ws://'+location.host);
    ws.onmessage=(e)=>handleData(JSON.parse(e.data));
    ws.onclose=()=>setTimeout(initWS,1000);
}

function saveMidiConfig(){
    let cfg={
        output:document.getElementById('midi-output').value,
        clock:document.getElementById('midi-clock').checked,
        smoothing:parseInt(document.getElementById('midi-smoothing').value)||0,
        latency:parseInt(document.getElementById('midi-latency').value)||0,
        noteCh:parseInt(document.getElementById('midi-note-ch').value)||0,
        beatNote:parseInt(document.getElementById('midi-beat-note').value)||0,
        downbeatNote:parseInt(document.getElementById('midi-downbeat').value)||0,
        ccCh:parseInt(document.getElementById('midi-cc-ch').value)||0,
        bpmCoarse:parseInt(document.getElementById('midi-bpm-coarse').value)||0,
        bpmFine:parseInt(document.getElementById('midi-bpm-fine').value)||0,
        pitchCc:parseInt(document.getElementById('midi-pitch-cc').value)||0,
        barPhaseCc:parseInt(document.getElementById('midi-bar-cc').value)||0,
        beatPhaseCc:parseInt(document.getElementById('midi-beat-cc').value)||0,
        playingCc:parseInt(document.getElementById('midi-playing-cc').value)||0
    };
    fetch('/api/set-midi',{method:'POST',body:JSON.stringify(cfg)}).catch(e=>console.error('Save error:',e));
}

initWS();
setInterval(updateMidiAnimations,50);
</script>
</body>
</html>"#
}

#[tokio::main]
async fn main() -> Result<()> {
    run_emulator().await
}
