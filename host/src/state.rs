//! Global DJ state — shared between the network listeners and MIDI output tasks.
//!
//! Consolidates beat packets, absolute-position packets, and status packets
//! into a single coherent view of the tempo master.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

// ── Beat source tag ───────────────────────────────────────────────────────────

/// Which data source last updated the master state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatSource {
    /// Pioneer Pro DJ Link (hardware CDJs/XDJs on Ethernet).
    ProLink,
    /// Ableton Link (rekordbox Performance mode or other Link peers).
    AbletonLink,
}

// ── Phrase / song structure ───────────────────────────────────────────────────

/// The mood classification rekordbox assigns to a track's phrase analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackMood {
    High,
    Mid,
    Low,
}

/// The kind of phrase within a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseKind {
    Intro,
    Verse1,
    Verse2,
    Verse3,
    Verse4,
    Verse5,
    Verse6,
    Bridge,
    Chorus,
    Up,
    Down,
    Outro,
    Unknown(u16),
}

impl std::fmt::Display for PhraseKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhraseKind::Intro => write!(f, "INTRO"),
            PhraseKind::Verse1 => write!(f, "VERSE 1"),
            PhraseKind::Verse2 => write!(f, "VERSE 2"),
            PhraseKind::Verse3 => write!(f, "VERSE 3"),
            PhraseKind::Verse4 => write!(f, "VERSE 4"),
            PhraseKind::Verse5 => write!(f, "VERSE 5"),
            PhraseKind::Verse6 => write!(f, "VERSE 6"),
            PhraseKind::Bridge => write!(f, "BRIDGE"),
            PhraseKind::Chorus => write!(f, "CHORUS"),
            PhraseKind::Up => write!(f, "UP"),
            PhraseKind::Down => write!(f, "DOWN"),
            PhraseKind::Outro => write!(f, "OUTRO"),
            PhraseKind::Unknown(id) => write!(f, "?{id}"),
        }
    }
}

/// A single phrase entry from the song structure analysis.
#[derive(Debug, Clone)]
pub struct PhraseEntry {
    /// 1-based index of this phrase.
    pub index: u16,
    /// Beat number at which this phrase starts.
    pub beat: u16,
    /// The kind of phrase.
    pub kind: PhraseKind,
    /// Whether a fill-in is present at the end of the phrase.
    pub has_fill: bool,
    /// Beat number at which fill-in starts (0 if no fill).
    pub fill_beat: u16,
}

/// Complete song structure for a track.
#[derive(Debug, Clone)]
pub struct SongStructure {
    pub mood: TrackMood,
    /// Beat number at which the last phrase ends.
    pub end_beat: u16,
    /// Ordered list of phrases.
    pub phrases: Vec<PhraseEntry>,
}

// ── Track-change notification ─────────────────────────────────────────────────

/// Emitted when a device loads a different track so the metadata fetcher can
/// query the dbserver for title/artist/key.
#[derive(Debug, Clone)]
pub struct TrackChange {
    /// The CDJ that changed track.
    pub device_number: u8,
    /// Device the track was loaded from (Dr).
    pub track_source_player: u8,
    /// Media slot (Sr).
    pub track_slot: u8,
    /// Track type (Tr).
    pub track_type: u8,
    /// Rekordbox database ID.
    pub rekordbox_id: u32,
}

// ── Per-device state ──────────────────────────────────────────────────────────

/// Everything we know about a single CDJ/mixer on the network.
#[derive(Debug, Clone)]
pub struct DeviceState {
    pub device_number: u8,
    /// True when this device is the Pro DJ Link tempo master.
    pub is_master: bool,
    /// True when the device is currently playing (OR of play_state byte and state flags bit).
    pub is_playing: bool,
    /// True when the channel is audible in the mix.
    pub is_on_air: bool,
    /// True when sync is enabled on this deck.
    pub is_sync: bool,
    /// Raw play state byte from status packet.
    pub play_state: crate::prolink::packets::PlayState,
    /// Current effective BPM (pitch-adjusted).  0.0 if not playing.
    pub effective_bpm: f64,
    /// Raw pitch as percent (-100.0 … +100.0).
    pub pitch_pct: f64,
    /// Beat within bar (1–4).  0 if unknown.
    pub beat_in_bar: u8,
    /// Absolute beat count from track start (0xFFFF_FFFF if unavailable).
    pub beat_count: u32,
    /// Playhead in ms (from AbsPosition packets; CDJ-3000 only).
    pub playhead_ms: Option<u32>,
    pub bar_phase: f64,
    pub beat_phase: f64,
    pub phrase_16_beat: u8,
    pub last_beat_at: Option<Instant>,
    /// BPM smoothing buffer: ring of recent effective BPM readings.
    bpm_history: VecDeque<f64>,

    // ── Track metadata (populated via dbserver TCP queries) ───────────────
    /// Rekordbox database ID of the currently loaded track (0 = none).
    pub rekordbox_id: u32,
    /// Slot the track was loaded from (mirrors CdjStatus::track_slot).
    pub track_slot: u8,
    /// Track type (mirrors CdjStatus::track_type).
    pub track_type: u8,
    /// Device the track was loaded from (mirrors CdjStatus::track_source_player).
    pub track_source_player: u8,
    /// Track title from dbserver metadata query.
    pub track_title: String,
    /// Artist name from dbserver metadata query.
    pub track_artist: String,
    /// Musical key from dbserver metadata query.
    pub track_key: String,
    /// Track BPM from dbserver metadata query (original, not pitch-adjusted).
    pub track_bpm_meta: Option<f64>,

    // ── Phrase / song structure ───────────────────────────────────────────
    /// Song structure (phrase analysis) fetched from the dbserver.
    pub song_structure: Option<SongStructure>,
    /// Index of the current phrase (into song_structure.phrases), if known.
    pub current_phrase_idx: Option<usize>,
    /// Previous phrase index — used to detect phrase changes.
    pub prev_phrase_idx: Option<usize>,
}

impl DeviceState {
    fn new(device_number: u8) -> Self {
        Self {
            device_number,
            is_master: false,
            is_playing: false,
            is_on_air: false,
            is_sync: false,
            play_state: crate::prolink::packets::PlayState::NoTrack,
            effective_bpm: 0.0,
            pitch_pct: 0.0,
            beat_in_bar: 0,
            beat_count: u32::MAX,
            playhead_ms: None,
            bar_phase: 0.0,
            beat_phase: 0.0,
            phrase_16_beat: 0,
            last_beat_at: None,
            bpm_history: VecDeque::with_capacity(8),
            rekordbox_id: 0,
            track_slot: 0,
            track_type: 0,
            track_source_player: 0,
            track_title: String::new(),
            track_artist: String::new(),
            track_key: String::new(),
            track_bpm_meta: None,
            song_structure: None,
            current_phrase_idx: None,
            prev_phrase_idx: None,
        }
    }

    /// Push a new BPM reading and return the smoothed value.
    fn smooth_bpm(&mut self, raw: f64, window: usize) -> f64 {
        if window == 0 || raw <= 0.0 {
            return raw;
        }
        self.bpm_history.push_back(raw);
        while self.bpm_history.len() > window {
            self.bpm_history.pop_front();
        }
        self.bpm_history.iter().sum::<f64>() / self.bpm_history.len() as f64
    }
}

// ── Master state (the view seen by MIDI tasks) ────────────────────────────────

/// The distilled state of the tempo master — the only view MIDI tasks need.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MasterState {
    /// Device number of the current tempo master (0 = none / Link).
    pub device_number: u8,
    /// Which source provided this state.
    pub source: Option<BeatSource>,
    /// Smoothed effective BPM.
    pub bpm: f64,
    /// Pitch percent.
    pub pitch_pct: f64,
    /// Beat within bar (1–4).
    pub beat_in_bar: u8,
    /// Phase within bar (0.0–1.0).
    pub bar_phase: f64,
    /// Phase within beat (0.0–1.0).
    pub beat_phase: f64,
    /// True when the master deck is playing.
    pub is_playing: bool,
    /// True when on-air.
    pub is_on_air: bool,
    /// Timestamp of the most recent beat event from the master.
    pub last_beat_at: Option<Instant>,
    pub is_virtual_master: bool,
    pub phrase_16_beat: u8,
}

impl Default for MasterState {
    fn default() -> Self {
        Self {
            device_number: 0,
            source: None,
            bpm: 0.0,
            pitch_pct: 0.0,
            beat_in_bar: 0,
            bar_phase: 0.0,
            beat_phase: 0.0,
            is_playing: false,
            is_on_air: false,
            last_beat_at: None,
            is_virtual_master: false,
            phrase_16_beat: 0,
        }
    }
}

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct DjState {
    /// Per-device state keyed by device number.
    pub devices: HashMap<u8, DeviceState>,
    /// Smoothed view of the master deck, pre-computed on every update.
    pub master: MasterState,
    /// BPM smoothing window (number of readings to average).
    pub bpm_smooth_window: usize,
    /// Cached device number of the current Pro DJ Link master (0 = none).
    /// Updated explicitly when `is_master` flags change, avoiding
    /// nondeterministic `HashMap::values().find()` iteration.
    master_device_num: u8,
    pub prolink_seen: bool,
    pub link_peer_count: usize,
}

impl DjState {
    pub fn new(smoothing_ms: u64) -> Self {
        // Convert smoothing_ms to a number of BPM readings.
        // Status packets arrive ~5/s; beat packets arrive at the beat rate.
        // We use ~5 readings per second as the baseline.
        let window = ((smoothing_ms as f64 / 200.0).round() as usize).max(1);
        Self {
            devices: HashMap::new(),
            master: MasterState::default(),
            bpm_smooth_window: window,
            master_device_num: 0,
            prolink_seen: false,
            link_peer_count: 0,
        }
    }

    pub fn set_smoothing_ms(&mut self, smoothing_ms: u64) {
        self.bpm_smooth_window = ((smoothing_ms as f64 / 200.0).round() as usize).max(1);
    }

    fn device_mut(&mut self, num: u8) -> &mut DeviceState {
        self.devices
            .entry(num)
            .or_insert_with(|| DeviceState::new(num))
    }

    /// Apply a CDJ status update.  Returns a [`TrackChange`] when the loaded
    /// track differs from the previous one (so the metadata fetcher can fire).
    pub fn apply_cdj_status(
        &mut self,
        s: &crate::prolink::packets::CdjStatus,
    ) -> Option<TrackChange> {
        let window = self.bpm_smooth_window;
        let dev = self.device_mut(s.device_number);
        dev.is_master = s.is_master;
        dev.is_playing = s.play_state.is_playing() || s.is_playing_flag;
        dev.is_on_air = s.is_on_air;
        dev.is_sync = s.is_sync;
        dev.play_state = s.play_state;
        dev.pitch_pct = s.pitch_pct;
        if dev.beat_in_bar != s.beat_in_bar && s.beat_in_bar != 0 {
            dev.phrase_16_beat = dev.phrase_16_beat.wrapping_add(1) % 16;
        }
        if s.beat_count != u32::MAX && s.beat_count > 0 {
            dev.phrase_16_beat = ((s.beat_count - 1) % 16) as u8;
        }

        dev.beat_in_bar = s.beat_in_bar;
        dev.beat_count = s.beat_count;
        if s.effective_bpm > 0.0 {
            dev.effective_bpm = dev.smooth_bpm(s.effective_bpm, window);
        }
        if s.beat_in_bar >= 1 {
            dev.bar_phase = ((dev.phrase_16_beat as f64 + dev.beat_phase) / 16.0).clamp(0.0, 1.0);
        }

        let prev_rbid = dev.rekordbox_id;
        let prev_slot = dev.track_slot;
        let prev_type = dev.track_type;
        let prev_src = dev.track_source_player;

        dev.track_source_player = s.track_source_player;
        dev.track_slot = s.track_slot;
        dev.track_type = s.track_type;
        dev.rekordbox_id = s.rekordbox_id;

        let track_changed = prev_rbid != s.rekordbox_id
            || prev_slot != s.track_slot
            || prev_type != s.track_type
            || prev_src != s.track_source_player;

        let change = if track_changed {
            // Clear stale metadata until the fetcher fills it in.
            dev.track_title.clear();
            dev.track_artist.clear();
            dev.track_key.clear();
            dev.track_bpm_meta = None;
            dev.song_structure = None;
            dev.current_phrase_idx = None;
            dev.prev_phrase_idx = None;
            dev.rekordbox_id = s.rekordbox_id;
            Some(TrackChange {
                device_number: s.device_number,
                track_source_player: s.track_source_player,
                track_slot: s.track_slot,
                track_type: s.track_type,
                rekordbox_id: s.rekordbox_id,
            })
        } else {
            if s.rekordbox_id == 0 && dev.rekordbox_id != 0 {
                // Track was ejected / unloaded.
                dev.rekordbox_id = 0;
                dev.track_title.clear();
                dev.track_artist.clear();
                dev.track_key.clear();
                dev.track_bpm_meta = None;
                dev.song_structure = None;
                dev.current_phrase_idx = None;
                dev.prev_phrase_idx = None;
            }
            dev.rekordbox_id = s.rekordbox_id;
            None
        };

        // Track master device explicitly.
        if s.is_master {
            if self.master_device_num != s.device_number {
                tracing::info!(
                    device = s.device_number,
                    prev_master = self.master_device_num,
                    "Master changed to device"
                );
            }
            self.master_device_num = s.device_number;
        } else if self.master_device_num == s.device_number {
            tracing::info!(device = s.device_number, "Device gave up master role");
            self.master_device_num = 0;
        }

        // Update phrase position based on current beat_count.
        let phrase_changed = self.update_current_phrase(s.device_number);
        if phrase_changed {
            if let Some(dev) = self.devices.get(&s.device_number) {
                if let (Some(ss), Some(idx)) = (&dev.song_structure, dev.current_phrase_idx) {
                    if let Some(phrase) = ss.phrases.get(idx) {
                        tracing::debug!(
                            device = s.device_number,
                            phrase = %phrase.kind,
                            index = phrase.index,
                            beat = phrase.beat,
                            "Phrase changed"
                        );
                    }
                }
            }
        }

        // Log all available deck info at DEBUG level.
        {
            let dev = self.devices.get(&s.device_number).unwrap();
            tracing::debug!(
                device = dev.device_number,
                is_master = dev.is_master,
                is_playing = dev.is_playing,
                is_on_air = dev.is_on_air,
                is_sync = dev.is_sync,
                play_state = ?dev.play_state,
                effective_bpm = format!("{:.2}", dev.effective_bpm).as_str(),
                pitch_pct = format!("{:.2}", dev.pitch_pct).as_str(),
                beat_in_bar = dev.beat_in_bar,
                beat_count = dev.beat_count,
                bar_phase = format!("{:.3}", dev.bar_phase).as_str(),
                beat_phase = format!("{:.3}", dev.beat_phase).as_str(),
                rekordbox_id = dev.rekordbox_id,
                track_slot = dev.track_slot,
                track_type = dev.track_type,
                track_source_player = dev.track_source_player,
                track_title = dev.track_title.as_str(),
                track_artist = dev.track_artist.as_str(),
                track_key = dev.track_key.as_str(),
                has_song_structure = dev.song_structure.is_some(),
                current_phrase_idx = ?dev.current_phrase_idx,
                "CDJ status update"
            );
        }

        self.refresh_master();
        change
    }

    /// Apply a mixer status update.
    pub fn apply_mixer_status(&mut self, s: &crate::prolink::packets::MixerStatus) {
        let dev = self.device_mut(s.device_number);
        dev.is_master = s.is_master;
        dev.beat_in_bar = s.beat_in_bar;
        if let Some(b) = s.track_bpm {
            dev.effective_bpm = b;
        }
        if s.is_master {
            if self.master_device_num != s.device_number {
                tracing::info!(
                    mixer_device = s.device_number,
                    prev_master = self.master_device_num,
                    "Mixer became master"
                );
            }
            self.master_device_num = s.device_number;
        } else if self.master_device_num == s.device_number {
            tracing::info!(mixer_device = s.device_number, "Mixer gave up master role");
            self.master_device_num = 0;
        }
        self.refresh_master();
    }

    /// Apply a beat packet.  Returns true if this was from the master deck.
    pub fn apply_beat(&mut self, bp: &crate::prolink::packets::BeatPacket) -> bool {
        let window = self.bpm_smooth_window;
        let dev = self.device_mut(bp.device_number);

        if dev.beat_in_bar != bp.beat_in_bar && bp.beat_in_bar != 0 {
            dev.phrase_16_beat = dev.phrase_16_beat.wrapping_add(1) % 16;
            if dev.beat_count != u32::MAX && dev.beat_count > 0 {
                dev.phrase_16_beat = ((dev.beat_count - 1) % 16) as u8;
            }
        }

        dev.beat_in_bar = bp.beat_in_bar;
        dev.last_beat_at = Some(Instant::now());
        if bp.effective_bpm > 0.0 {
            dev.effective_bpm = dev.smooth_bpm(bp.effective_bpm, window);
            let beat_dur_ms = 60_000.0 / bp.effective_bpm;
            let time_into_beat = (beat_dur_ms - bp.next_beat_ms as f64).clamp(0.0, beat_dur_ms);
            dev.beat_phase = (time_into_beat / beat_dur_ms).clamp(0.0, 1.0);
            dev.bar_phase = ((dev.phrase_16_beat as f64 + dev.beat_phase) / 16.0).clamp(0.0, 1.0);
        }
        dev.pitch_pct = bp.pitch_pct;
        let is_master = self.master.device_number == bp.device_number;
        self.refresh_master();
        is_master
    }

    /// Apply a CDJ-3000 absolute-position packet.  Returns true if master.
    pub fn apply_abs_position(&mut self, ap: &crate::prolink::packets::AbsPositionPacket) -> bool {
        let window = self.bpm_smooth_window;
        let dev = self.device_mut(ap.device_number);
        dev.playhead_ms = Some(ap.playhead_ms);
        dev.pitch_pct = ap.pitch_pct;
        if ap.effective_bpm > 0.0 {
            dev.effective_bpm = dev.smooth_bpm(ap.effective_bpm, window);
        }
        // Compute fine beat phase from playhead and BPM.
        // beat_duration_ms = 60_000 / bpm
        // beat_phase = (playhead % beat_duration) / beat_duration
        if ap.effective_bpm > 0.0 {
            let beat_dur = 60_000.0 / ap.effective_bpm;
            let within = (ap.playhead_ms as f64) % beat_dur;
            dev.beat_phase = (within / beat_dur).clamp(0.0, 1.0);
        }
        let is_master = self.master.device_number == ap.device_number;
        self.refresh_master();
        is_master
    }

    /// Recompute the master snapshot from the cached master device number.
    /// If no device explicitly claims master, falls back to the first playing
    fn refresh_master(&mut self) {
        let prev_master = self.master.device_number;

        let (dev, is_virtual, selection_reason) = if self.master_device_num != 0 {
            let reason = if let Some(d) = self.devices.get(&self.master_device_num) {
                format!(
                    "explicit is_master (device {} claims master, is_master={}, is_playing={}, bpm={:.2})",
                    d.device_number, d.is_master, d.is_playing, d.effective_bpm
                )
            } else {
                format!(
                    "explicit master_device_num={} but not in device table",
                    self.master_device_num
                )
            };
            (self.devices.get(&self.master_device_num), false, reason)
        } else {
            let mixer = self
                .devices
                .values()
                .filter(|d| d.device_number >= 16 && d.effective_bpm > 0.0)
                .min_by_key(|d| d.device_number);
            if let Some(d) = mixer {
                let reason = format!(
                    "fallback(mixer) → device {} (bpm={:.2})",
                    d.device_number, d.effective_bpm
                );
                (Some(d), true, reason)
            } else {
                let real_cdj = self
                    .devices
                    .values()
                    .filter(|d| d.device_number < 16 && d.effective_bpm > 0.0)
                    .min_by_key(|d| d.device_number);
                if let Some(d) = real_cdj {
                    let reason = format!(
                        "fallback(cdj) → device {} (is_playing={}, bpm={:.2})",
                        d.device_number, d.is_playing, d.effective_bpm
                    );
                    (Some(d), true, reason)
                } else {
                    (None, false, "no devices with bpm>0".to_string())
                }
            }
        };

        if let Some(dev) = dev {
            if prev_master != dev.device_number || !self.master.source.is_some() {
                tracing::info!(
                    device = dev.device_number,
                    is_virtual_master = is_virtual,
                    reason = %selection_reason,
                    "Master selected"
                );
            }
            self.master = MasterState {
                device_number: dev.device_number,
                source: Some(BeatSource::ProLink),
                bpm: dev.effective_bpm,
                pitch_pct: dev.pitch_pct,
                beat_in_bar: dev.beat_in_bar,
                bar_phase: dev.bar_phase,
                beat_phase: dev.beat_phase,
                is_playing: dev.is_playing,
                is_on_air: dev.is_on_air,
                last_beat_at: dev.last_beat_at,
                is_virtual_master: is_virtual,
                phrase_16_beat: dev.phrase_16_beat,
            };
        } else if self.master_device_num == 0 {
            if prev_master != 0 {
                tracing::info!("Master cleared (no devices with bpm>0)");
            }
            self.master = MasterState::default();
        }
    }

    /// Apply a beat/phase/tempo snapshot from the Ableton Link engine.
    ///
    /// `beat_in_bar` is 1-based (1–quantum).
    /// `bar_phase` is 0.0–1.0 (position within the bar).
    /// `beat_phase` is 0.0–1.0 (position within the current beat).
    pub fn apply_link_state(
        &mut self,
        bpm: f64,
        beat_in_bar: u8,
        bar_phase: f64,
        beat_phase: f64,
        is_playing: bool,
        beat_crossed: bool,
    ) {
        // Priority logic for "Auto" source mode:
        // 1. If we have active Link peers, Link ALWAYS takes priority.
        // 2. If no Link peers, but we have a ProLink master, ProLink takes priority.
        // 3. Otherwise, Link can fill the silence (e.g. if started first).

        let has_link_peers = self.link_peer_count > 0;
        let prolink_active =
            self.master.source == Some(BeatSource::ProLink) && self.master.bpm > 0.0;

        if prolink_active && !has_link_peers {
            return;
        }

        self.master = MasterState {
            device_number: 0,
            source: Some(BeatSource::AbletonLink),
            bpm,
            pitch_pct: 0.0,
            beat_in_bar,
            bar_phase,
            beat_phase,
            is_playing,
            is_on_air: is_playing,
            last_beat_at: if beat_crossed {
                Some(Instant::now())
            } else {
                self.master.last_beat_at
            },
            is_virtual_master: false,
            phrase_16_beat: 0,
        };
    }

    /// Remove a device (e.g. on disconnect).
    pub fn remove_device(&mut self, num: u8) {
        self.devices.remove(&num);
        if self.master_device_num == num {
            self.master_device_num = 0;
            self.master = MasterState::default();
        }
    }

    /// Record that at least one real Pro DJ Link device has been seen on the network.
    pub fn mark_prolink_seen(&mut self) {
        self.prolink_seen = true;
    }

    pub fn set_link_peer_count(&mut self, count: usize) {
        if self.link_peer_count != count {
            tracing::info!(
                old = self.link_peer_count,
                new = count,
                "Ableton Link peer count changed"
            );
        }
        self.link_peer_count = count;
        self.refresh_master();
    }

    /// Set track metadata received from a dbserver query.
    /// Only applies if the `rekordbox_id` still matches (avoids stale writes
    /// from a slow query that raced against a track change).
    pub fn set_track_metadata(
        &mut self,
        device_number: u8,
        rekordbox_id: u32,
        title: String,
        artist: String,
        key: String,
        bpm: Option<f64>,
    ) {
        if let Some(dev) = self.devices.get_mut(&device_number) {
            if dev.rekordbox_id == rekordbox_id {
                dev.track_title = title;
                dev.track_artist = artist;
                dev.track_key = key;
                dev.track_bpm_meta = bpm;
            }
        }
    }

    /// Set the song structure (phrase analysis) for a device.
    /// Only applies if the `rekordbox_id` still matches.
    pub fn set_song_structure(
        &mut self,
        device_number: u8,
        rekordbox_id: u32,
        structure: SongStructure,
    ) {
        if let Some(dev) = self.devices.get_mut(&device_number) {
            if dev.rekordbox_id == rekordbox_id {
                dev.song_structure = Some(structure);
                dev.current_phrase_idx = None;
                dev.prev_phrase_idx = None;
            }
        }
    }

    /// Update the current phrase index for a device based on its beat_count.
    /// Returns `true` if the phrase changed (for MIDI note trigger).
    pub fn update_current_phrase(&mut self, device_number: u8) -> bool {
        let dev = match self.devices.get_mut(&device_number) {
            Some(d) => d,
            None => return false,
        };
        let beat_count = dev.beat_count;
        if beat_count == u32::MAX {
            return false;
        }
        let structure = match &dev.song_structure {
            Some(s) => s,
            None => return false,
        };
        // beat_count is 0-indexed from status packets; phrase beats are 1-indexed.
        // Status packet beat_count starts at 1 for the first beat.
        let beat = beat_count as u16;

        // Find which phrase we're in: the last phrase whose start beat <= current beat.
        let new_idx = structure
            .phrases
            .iter()
            .enumerate()
            .rev()
            .find(|(_, p)| p.beat <= beat)
            .map(|(i, _)| i);

        let changed = dev.current_phrase_idx != new_idx && new_idx.is_some();
        dev.prev_phrase_idx = dev.current_phrase_idx;
        dev.current_phrase_idx = new_idx;
        changed
    }
}

/// The Arc-wrapped, RwLock-protected shared state used across async tasks.
pub type SharedState = Arc<RwLock<DjState>>;

pub fn new_shared(smoothing_ms: u64) -> SharedState {
    Arc::new(RwLock::new(DjState::new(smoothing_ms)))
}
