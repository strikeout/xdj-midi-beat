use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use embassy_sync::blocking_mutex::CriticalSectionMutex;
use embassy_sync::mutex::Mutex as EmbassyMutex;

pub struct DjState {
    pub master_bpm: AtomicU64,
    pub master_pitch: AtomicU64,
    pub master_beat: AtomicU8,
    pub master_bar_phase: AtomicU64,
    pub master_beat_phase: AtomicU64,
    pub master_is_playing: AtomicBool,
    pub master_device: AtomicU8,
    pub source: AtomicU8,
    pub prolink_devices: UnsafeCell<[DeviceInfo; 16]>,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub device_number: u8,
    pub is_master: bool,
    pub is_playing: bool,
    pub bpm: f64,
    pub name: heapless::String<16>,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self {
            device_number: 0,
            is_master: false,
            is_playing: false,
            bpm: 0.0,
            name: heapless::String::new(),
        }
    }
}

impl DjState {
    pub fn new() -> Self {
        Self {
            master_bpm: AtomicU64::new(0),
            master_pitch: AtomicU64::new(0),
            master_beat: AtomicU8::new(0),
            master_bar_phase: AtomicU64::new(0),
            master_beat_phase: AtomicU64::new(0),
            master_is_playing: AtomicBool::new(false),
            master_device: AtomicU8::new(0),
            source: AtomicU8::new(0),
            prolink_devices: UnsafeCell::new([DeviceInfo::default(); 16]),
        }
    }

    pub fn apply_beat(&self, device: u8, bpm: f64, beat: u8, pitch: f64) {
        if device == self.master_device.load(Ordering::Relaxed)
            || self.master_device.load(Ordering::Relaxed) == 0
        {
            self.master_bpm
                .store((bpm * 100.0) as u64, Ordering::Relaxed);
            self.master_beat.store(beat, Ordering::Relaxed);
            self.master_pitch
                .store((pitch * 100.0) as u64, Ordering::Relaxed);
        }
    }

    pub fn apply_cdj_status(
        &self,
        device: u8,
        is_master: bool,
        bpm: f64,
        beat: u8,
        is_playing: bool,
    ) {
        if is_master {
            self.master_device.store(device, Ordering::Relaxed);
            self.master_bpm
                .store((bpm * 100.0) as u64, Ordering::Relaxed);
            self.master_beat.store(beat, Ordering::Relaxed);
            self.master_is_playing.store(is_playing, Ordering::Relaxed);
        }
    }

    pub fn get_master_bpm(&self) -> f64 {
        self.master_bpm.load(Ordering::Relaxed) as f64 / 100.0
    }

    pub fn get_master_beat(&self) -> u8 {
        self.master_beat.load(Ordering::Relaxed)
    }

    pub fn get_master_is_playing(&self) -> bool {
        self.master_is_playing.load(Ordering::Relaxed)
    }

    pub fn get_master_device(&self) -> u8 {
        self.master_device.load(Ordering::Relaxed)
    }
}

pub type SharedState = &'static DjState;

pub fn new_shared() -> SharedState {
    heapless::singleton::Smp::new(DjState::new())
}
