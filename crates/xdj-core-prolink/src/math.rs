use crate::{BPM_NONE, PITCH_NORMAL};

#[inline]
pub fn pitch_to_percent(raw: u32) -> f64 {
    (raw as f64 - PITCH_NORMAL as f64) / PITCH_NORMAL as f64 * 100.0
}

#[inline]
pub fn percent_to_pitch(pct: f64) -> u32 {
    ((pct / 100.0 + 1.0) * PITCH_NORMAL as f64) as u32
}

#[inline]
pub fn bpm_from_raw(raw: u16) -> Option<f64> {
    if raw == BPM_NONE {
        None
    } else {
        Some(raw as f64 / 100.0)
    }
}

#[inline]
pub fn effective_bpm(track_bpm: f64, pitch_raw: u32) -> f64 {
    track_bpm * pitch_raw as f64 / PITCH_NORMAL as f64
}
