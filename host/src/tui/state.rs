//! TUI-specific state — UI selections, log ring buffer, activity counters.
//!
//! Separate from `DjState` because this tracks UI concerns (selected port,
//! scroll offsets, animation timers) that the audio/MIDI pipeline should
//! not know about.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use network_interface::{NetworkInterface, NetworkInterfaceConfig, V4IfAddr};
use parking_lot::Mutex;

use crate::config::{Config, Source};

// ── Log ring buffer (shared with tracing layer) ──────────────────────────────

/// Max log lines retained for the TUI log panel.
const LOG_CAPACITY: usize = 200;

/// Thread-safe ring buffer of log lines, written by the tracing layer and
/// read by the TUI renderer.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY))),
        }
    }

    /// Push a log line (called from the tracing writer).
    pub fn push(&self, line: String) {
        let mut buf = self.inner.lock();
        if buf.len() >= LOG_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    /// Snapshot all lines for rendering.
    pub fn lines(&self) -> Vec<String> {
        self.inner.lock().iter().cloned().collect()
    }
}

/// A `std::io::Write` implementation that routes tracing output into the
/// `LogBuffer` instead of stdout.  Each `write()` call appends to a
/// line buffer; newlines flush complete lines into the ring.
#[derive(Clone)]
pub struct LogWriter {
    buf: LogBuffer,
    partial: String,
}

impl LogWriter {
    pub fn new(buf: LogBuffer) -> Self {
        Self {
            buf,
            partial: String::new(),
        }
    }
}

impl std::io::Write for LogWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let chunk = String::from_utf8_lossy(data);
        self.partial.push_str(&chunk);

        let mut it = self.partial.split('\n').peekable();
        let mut new_partial: Option<String> = None;

        while let Some(part) = it.next() {
            let is_last = it.peek().is_none();
            if is_last {
                new_partial = Some(part.to_string());
                break;
            }

            let line = part.trim_end_matches('\r');
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                self.buf.push(trimmed.to_string());
            }
        }

        self.partial = new_partial.unwrap_or_default();
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn log_buffer_clones_share_inner_ring() {
        let a = LogBuffer::new();
        let b = a.clone();

        assert!(Arc::ptr_eq(&a.inner, &b.inner));

        b.push("hello".to_string());
        assert_eq!(a.lines(), vec!["hello".to_string()]);
    }

    #[test]
    fn log_writer_buffers_partial_lines_until_newline() {
        let buf = LogBuffer::new();
        let mut w = LogWriter::new(buf.clone());

        w.write_all(b"hello ").unwrap();
        assert_eq!(buf.lines(), Vec::<String>::new());

        w.write_all(b"world\n").unwrap();
        assert_eq!(buf.lines(), vec!["hello world".to_string()]);
    }
}

/// Wrapper that implements `tracing_subscriber::fmt::MakeWriter` so we can
/// plug `LogWriter` into the tracing subscriber.
#[derive(Clone)]
pub struct MakeLogWriter {
    writer: LogWriter,
}

impl MakeLogWriter {
    pub fn new(buf: LogBuffer) -> Self {
        Self {
            writer: LogWriter::new(buf),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeLogWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.writer.clone()
    }
}

// ── MIDI port info ───────────────────────────────────────────────────────────

/// A snapshot of an available MIDI output port.
#[derive(Debug, Clone)]
pub struct MidiPortInfo {
    /// Display name (from midir).
    pub name: String,
    /// Port index in the midir port list at the time of enumeration.
    pub index: usize,
}

// ── MIDI activity counters ───────────────────────────────────────────────────

/// Counters the TUI displays in the output panel.  Updated atomically from
/// MIDI tasks, read by the renderer.
#[derive(Debug, Clone)]
pub struct MidiActivity {
    pub clock_pulses: u64,
    pub mtc_quarter_frames: u64,
    pub mtc_full_frames: u64,
    pub notes_sent: u64,
    pub cc_sent: u64,
    /// Last note-on fired: (note_number, when).
    pub last_note: Option<(u8, Instant)>,
    /// Last CC sent: (cc_number, value, when).
    pub last_cc: Option<(u8, u8, Instant)>,

    pub clock_running: bool,
    pub clock_waiting_for_phrase: bool,
    pub clock_wait_beats_seen: u8,
    pub clock_phrase_beat: u8,
    pub clock_pulse_index: u64,
    pub clock_last_start_at: Option<Instant>,
    pub clock_last_pulse_at: Option<Instant>,
}

impl Default for MidiActivity {
    fn default() -> Self {
        Self {
            clock_pulses: 0,
            mtc_quarter_frames: 0,
            mtc_full_frames: 0,
            notes_sent: 0,
            cc_sent: 0,
            last_note: None,
            last_cc: None,

            clock_running: false,
            clock_waiting_for_phrase: false,
            clock_wait_beats_seen: 0,
            clock_phrase_beat: 0,
            clock_pulse_index: 0,
            clock_last_start_at: None,
            clock_last_pulse_at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkIfaceInfo {
    pub name: String,
    pub ip: String,
    pub priority: u8,
    pub priority_label: String,
}

/// Index into SETTINGS where the MIDI settings begin (indices 0..MIDI_SETTINGS_START
/// are network/device settings shown on the left; MIDI_SETTINGS_START.. are on the right).
pub const MIDI_SETTINGS_START: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePanel {
    MidiPorts,
    /// Network/device settings (left panel, indices 0..MIDI_SETTINGS_START).
    InputSettings,
    /// MIDI settings (right panel, indices MIDI_SETTINGS_START..).
    MidiSettings,
}

#[derive(Debug, Clone, Copy)]
pub enum SettingKind {
    CycleInterface,
    CycleSource,
    Toggle,
    NumericU8,
    NumericU64,
    NumericI64,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub label: &'static str,
    pub kind: SettingKind,
    pub section: Option<&'static str>,
}

pub const SETTINGS: &[SettingDef] = &[
    SettingDef {
        label: "Network Interface",
        kind: SettingKind::CycleInterface,
        section: None,
    },
    SettingDef {
        label: "Source Mode",
        kind: SettingKind::CycleSource,
        section: None,
    },
    SettingDef {
        label: "Device Number",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Clock Enabled",
        kind: SettingKind::Toggle,
        section: Some("MIDI Clock"),
    },
    SettingDef {
        label: "BPM Smoothing",
        kind: SettingKind::NumericU64,
        section: None,
    },
    SettingDef {
        label: "Latency Comp",
        kind: SettingKind::NumericI64,
        section: None,
    },
    SettingDef {
        label: "Phrase Lock Stable",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Note Channel",
        kind: SettingKind::NumericU8,
        section: Some("MIDI Notes"),
    },
    SettingDef {
        label: "Beat Note",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Downbeat Note",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Phrase Change Note",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "CC Channel",
        kind: SettingKind::NumericU8,
        section: Some("MIDI CC"),
    },
    SettingDef {
        label: "BPM Coarse CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "BPM Fine CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Pitch CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Bar Phase CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Beat Phase CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Playing CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Master Deck CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    SettingDef {
        label: "Phrase 16 CC",
        kind: SettingKind::NumericU8,
        section: None,
    },
    // ── MTC settings ─────────────────────────────────────────────────────
    SettingDef {
        label: "MTC Enabled",
        kind: SettingKind::Toggle,
        section: Some("MIDI Timecode"),
    },
    SettingDef {
        label: "MTC Frame Rate",
        kind: SettingKind::CycleSource, // reuse cycle kind for frame rate cycling
        section: None,
    },
];

// ── TUI state ────────────────────────────────────────────────────────────────

/// UI state that the render loop owns.  Not shared across threads — only the
/// TUI task reads/writes this.
pub struct TuiState {
    /// Available MIDI output ports (refreshed on startup / manual rescan).
    pub midi_ports: Vec<MidiPortInfo>,
    /// Index into `midi_ports` for the currently active port.
    pub active_port_idx: usize,
    /// Index into `midi_ports` for the cursor (arrow-key highlight).
    pub cursor_port_idx: usize,
    /// Which lower-left panel is active.
    pub active_panel: ActivePanel,
    /// Cursor into the editable settings list.
    pub settings_cursor: usize,
    /// Cached network interfaces for the settings UI.
    pub interfaces: Vec<NetworkIfaceInfo>,
    /// Whether the settings panel is editing a numeric value.
    pub editing: bool,
    /// Temporary numeric edit buffer.
    pub edit_buffer: String,
    /// Log ring buffer (shared with tracing layer).
    pub log_buf: LogBuffer,
    /// MIDI activity counters (shared with MIDI tasks).
    pub midi_activity: Arc<Mutex<MidiActivity>>,
    /// Instant of the most recent beat (for flash animation).
    pub last_beat_flash: Option<Instant>,
    /// Whether we should quit the TUI.
    pub should_quit: bool,
}

impl TuiState {
    pub fn new(log_buf: LogBuffer, midi_activity: Arc<Mutex<MidiActivity>>) -> Self {
        Self {
            midi_ports: Vec::new(),
            active_port_idx: 0,
            cursor_port_idx: 0,
            active_panel: ActivePanel::MidiPorts,
            settings_cursor: 0,
            interfaces: Vec::new(),
            editing: false,
            edit_buffer: String::new(),
            log_buf,
            midi_activity,
            last_beat_flash: None,
            should_quit: false,
        }
    }

    /// Refresh the MIDI port list from midir.  Returns the number of ports found.
    pub fn refresh_midi_ports(&mut self) -> usize {
        self.midi_ports.clear();
        if let Ok(midi_out) = midir::MidiOutput::new("xdj-clock-tui") {
            let ports = midi_out.ports();
            for (i, port) in ports.iter().enumerate() {
                if let Ok(name) = midi_out.port_name(port) {
                    self.midi_ports.push(MidiPortInfo { name, index: i });
                }
            }
        }
        if self.active_port_idx >= self.midi_ports.len() {
            self.active_port_idx = self.midi_ports.len().saturating_sub(1);
        }
        if self.cursor_port_idx >= self.midi_ports.len() {
            self.cursor_port_idx = self.midi_ports.len().saturating_sub(1);
        }
        self.midi_ports.len()
    }

    pub fn refresh_interfaces(&mut self) {
        self.interfaces.clear();

        let Ok(ifaces) = NetworkInterface::show() else {
            return;
        };

        for iface in &ifaces {
            for addr in &iface.addr {
                if let network_interface::Addr::V4(V4IfAddr { ip, .. }) = addr {
                    if ip.is_loopback() {
                        continue;
                    }

                    let priority = crate::app::interface_priority(&iface.name, ip);
                    self.interfaces.push(NetworkIfaceInfo {
                        name: iface.name.clone(),
                        ip: ip.to_string(),
                        priority,
                        priority_label: interface_priority_label(priority).to_string(),
                    });
                }
            }
        }

        self.interfaces.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(a.name.cmp(&b.name))
                .then(a.ip.cmp(&b.ip))
        });
    }

    #[allow(dead_code)]
    pub fn settings_count() -> usize {
        SETTINGS.len()
    }

    /// Move the port selector cursor up.
    pub fn cursor_up(&mut self) {
        if !self.midi_ports.is_empty() && self.cursor_port_idx > 0 {
            self.cursor_port_idx -= 1;
        }
    }

    /// Move the port selector cursor down.
    pub fn cursor_down(&mut self) {
        if !self.midi_ports.is_empty() && self.cursor_port_idx < self.midi_ports.len() - 1 {
            self.cursor_port_idx += 1;
        }
    }

    pub fn cursor_up_settings(&mut self) {
        let min = self.settings_panel_min();
        if self.settings_cursor > min {
            self.settings_cursor -= 1;
        }
    }

    pub fn cursor_down_settings(&mut self) {
        let max = self.settings_panel_max();
        if self.settings_cursor + 1 < max {
            self.settings_cursor += 1;
        }
    }

    /// Minimum SETTINGS index for the currently active settings panel.
    fn settings_panel_min(&self) -> usize {
        match self.active_panel {
            ActivePanel::InputSettings => 0,
            ActivePanel::MidiSettings => MIDI_SETTINGS_START,
            _ => 0,
        }
    }

    /// Exclusive upper bound of SETTINGS indices for the active panel.
    fn settings_panel_max(&self) -> usize {
        match self.active_panel {
            ActivePanel::InputSettings => MIDI_SETTINGS_START,
            ActivePanel::MidiSettings => SETTINGS.len(),
            _ => SETTINGS.len(),
        }
    }

    /// Returns true if the beat flash should be visible (< 100ms since last beat).
    pub fn beat_flash_active(&self) -> bool {
        self.last_beat_flash
            .map(|t| t.elapsed().as_millis() < 100)
            .unwrap_or(false)
    }
}

pub fn interface_priority_label(priority: u8) -> &'static str {
    match priority {
        0 => "DJ",
        1 => "Eth",
        2 => "WiFi",
        3 => "Unknown",
        4 => "LinkLocal",
        5 => "VPN",
        _ => "?",
    }
}

pub fn setting_kind(idx: usize) -> SettingKind {
    SETTINGS[idx].kind
}

pub fn selected_interface_index(cfg: &Config, interfaces: &[NetworkIfaceInfo]) -> Option<usize> {
    if interfaces.is_empty() {
        return None;
    }

    let wanted = cfg.interface.to_lowercase();
    interfaces
        .iter()
        .position(|iface| iface.name.to_lowercase() == wanted)
        .or_else(|| {
            if wanted == "auto" {
                Some(0)
            } else {
                interfaces
                    .iter()
                    .position(|iface| iface.name.to_lowercase().contains(&wanted))
            }
        })
        .or(Some(0))
}

pub fn format_interface_value(cfg: &Config, interfaces: &[NetworkIfaceInfo]) -> String {
    selected_interface_index(cfg, interfaces)
        .and_then(|idx| interfaces.get(idx))
        .map(|iface| {
            format!(
                "{} ({}) [prio {} {}]",
                iface.name, iface.ip, iface.priority, iface.priority_label
            )
        })
        .unwrap_or_else(|| cfg.interface.clone())
}

pub fn format_source_value(source: &Source) -> &'static str {
    match source {
        Source::Auto => "Auto",
        Source::ProLink => "ProLink (network)",
        Source::Link => "AbletonLink",
    }
}

pub fn get_value(cfg: &Config, interfaces: &[NetworkIfaceInfo], idx: usize) -> String {
    match idx {
        0 => format_interface_value(cfg, interfaces),
        1 => format_source_value(&cfg.source).to_string(),
        2 => cfg.device_number.to_string(),
        3 => {
            if cfg.midi.clock_enabled {
                "✓".to_string()
            } else {
                "✗".to_string()
            }
        }
        4 => format!("{} ms", cfg.midi.smoothing_ms),
        5 => format!("{} ms", cfg.midi.latency_compensation_ms),
        6 => format!("{} beats", cfg.midi.phrase_lock_stable_beats),
        7 => (cfg.midi.notes.channel + 1).to_string(),
        8 => cfg.midi.notes.beat.to_string(),
        9 => cfg.midi.notes.downbeat.to_string(),
        10 => cfg.midi.notes.phrase_change.to_string(),
        11 => (cfg.midi.cc.channel + 1).to_string(),
        12 => cfg.midi.cc.bpm_coarse.to_string(),
        13 => cfg.midi.cc.bpm_fine.to_string(),
        14 => cfg.midi.cc.pitch.to_string(),
        15 => cfg.midi.cc.bar_phase.to_string(),
        16 => cfg.midi.cc.beat_phase.to_string(),
        17 => cfg.midi.cc.playing.to_string(),
        18 => cfg.midi.cc.master_deck.to_string(),
        19 => cfg.midi.cc.phrase_16.to_string(),
        20 => {
            if cfg.midi.mtc.enabled {
                "✓".to_string()
            } else {
                "✗".to_string()
            }
        }
        21 => cfg.midi.mtc.frame_rate.label().to_string(),
        _ => String::new(),
    }
}

pub fn numeric_edit_value(cfg: &Config, idx: usize) -> Option<String> {
    match setting_kind(idx) {
        SettingKind::NumericU8 | SettingKind::NumericU64 => Some(match idx {
            2 => cfg.device_number.to_string(),
            4 => cfg.midi.smoothing_ms.to_string(),
            6 => cfg.midi.phrase_lock_stable_beats.to_string(),
            7 => (cfg.midi.notes.channel + 1).to_string(),
            8 => cfg.midi.notes.beat.to_string(),
            9 => cfg.midi.notes.downbeat.to_string(),
            10 => cfg.midi.notes.phrase_change.to_string(),
            11 => (cfg.midi.cc.channel + 1).to_string(),
            12 => cfg.midi.cc.bpm_coarse.to_string(),
            13 => cfg.midi.cc.bpm_fine.to_string(),
            14 => cfg.midi.cc.pitch.to_string(),
            15 => cfg.midi.cc.bar_phase.to_string(),
            16 => cfg.midi.cc.beat_phase.to_string(),
            17 => cfg.midi.cc.playing.to_string(),
            18 => cfg.midi.cc.master_deck.to_string(),
            19 => cfg.midi.cc.phrase_16.to_string(),
            _ => return None,
        }),
        SettingKind::NumericI64 => Some(match idx {
            5 => cfg.midi.latency_compensation_ms.to_string(),
            _ => return None,
        }),
        _ => None,
    }
}

pub fn apply_change(
    cfg: &mut Config,
    interfaces: &[NetworkIfaceInfo],
    idx: usize,
    direction: i8,
) -> bool {
    match idx {
        0 => {
            let Some(current) = selected_interface_index(cfg, interfaces) else {
                return false;
            };
            let len = interfaces.len();
            if len == 0 {
                return false;
            }
            let next = if direction < 0 {
                if current == 0 {
                    len - 1
                } else {
                    current - 1
                }
            } else {
                (current + 1) % len
            };
            if let Some(iface) = interfaces.get(next) {
                cfg.interface = iface.name.clone();
                return true;
            }
            false
        }
        1 => {
            cfg.source = match (cfg.source.clone(), direction < 0) {
                (Source::Auto, false) => Source::ProLink,
                (Source::ProLink, false) => Source::Link,
                (Source::Link, false) => Source::Auto,
                (Source::Auto, true) => Source::Link,
                (Source::Link, true) => Source::ProLink,
                (Source::ProLink, true) => Source::Auto,
            };
            true
        }
        3 => {
            cfg.midi.clock_enabled = !cfg.midi.clock_enabled;
            true
        }
        5 => {
            let step = if direction < 0 { -5 } else { 5 };
            cfg.midi.latency_compensation_ms =
                (cfg.midi.latency_compensation_ms + step).clamp(-1000, 1000);
            true
        }
        6 => {
            let step = if direction < 0 { -1 } else { 1 };
            let next = (cfg.midi.phrase_lock_stable_beats as i16 + step).clamp(1, 32) as u8;
            cfg.midi.phrase_lock_stable_beats = next;
            true
        }
        20 => {
            cfg.midi.mtc.enabled = !cfg.midi.mtc.enabled;
            true
        }
        21 => {
            cfg.midi.mtc.frame_rate = cfg.midi.mtc.frame_rate.next();
            true
        }
        _ => false,
    }
}

pub fn apply_numeric_input(cfg: &mut Config, idx: usize, value: &str) -> bool {
    match idx {
        2 => value
            .parse::<u8>()
            .ok()
            .filter(|v| (1..=15).contains(v))
            .map(|v| cfg.device_number = v)
            .is_some(),
        4 => value
            .parse::<u64>()
            .ok()
            .filter(|v| *v <= 1000)
            .map(|v| cfg.midi.smoothing_ms = v)
            .is_some(),
        5 => value
            .parse::<i64>()
            .ok()
            .filter(|v| (-1000..=1000).contains(v))
            .map(|v| cfg.midi.latency_compensation_ms = v)
            .is_some(),
        6 => value
            .parse::<u8>()
            .ok()
            .filter(|v| (1..=32).contains(v))
            .map(|v| cfg.midi.phrase_lock_stable_beats = v)
            .is_some(),
        7 => value
            .parse::<u8>()
            .ok()
            .filter(|v| (1..=16).contains(v))
            .map(|v| cfg.midi.notes.channel = v - 1)
            .is_some(),
        8 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.notes.beat = v)
            .is_some(),
        9 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.notes.downbeat = v)
            .is_some(),
        10 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.notes.phrase_change = v)
            .is_some(),
        11 => value
            .parse::<u8>()
            .ok()
            .filter(|v| (1..=16).contains(v))
            .map(|v| cfg.midi.cc.channel = v - 1)
            .is_some(),
        12 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.bpm_coarse = v)
            .is_some(),
        13 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.bpm_fine = v)
            .is_some(),
        14 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.pitch = v)
            .is_some(),
        15 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.bar_phase = v)
            .is_some(),
        16 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.beat_phase = v)
            .is_some(),
        17 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.playing = v)
            .is_some(),
        18 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.master_deck = v)
            .is_some(),
        19 => value
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 127)
            .map(|v| cfg.midi.cc.phrase_16 = v)
            .is_some(),
        _ => false,
    }
}


pub fn selected_interface_name_ip(cfg: &Config, interfaces: &[NetworkIfaceInfo]) -> String {
    selected_interface_index(cfg, interfaces)
        .and_then(|idx| interfaces.get(idx))
        .map(|iface| format!("{} ({})", iface.name, iface.ip))
        .unwrap_or_else(|| cfg.interface.clone())
}
