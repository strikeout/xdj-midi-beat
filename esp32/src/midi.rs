use core::mem::MaybeUninit;
use embedded_hal::serial::Write;
use esp_idf_hal::gpio::GpioPin;
use esp_idf_hal::uart::{UartConfig, UartDriver};

const MIDI_BAUD: u32 = 31250;

pub struct MidiInterface {
    uart: UartDriver<'static>,
}

impl MidiInterface {
    pub fn new(uart: UartDriver<'static>) -> Self {
        Self { uart }
    }

    pub fn read_byte(&mut self) -> Option<u8> {
        let mut byte: MaybeUninit<u8> = MaybeUninit::uninit();
        match self.uart.read_byte(unsafe { byte.as_mut_ptr() }) {
            Ok(_) => Some(unsafe { byte.assume_init() }),
            Err(_) => None,
        }
    }

    pub fn write_message(&mut self, msg: &[u8]) {
        for byte in msg {
            let _ = self.uart.write(*byte);
        }
        let _ = self.uart.flush();
    }

    pub fn parse_midi_message(&mut self) -> Option<MidiMessage> {
        let byte = self.read_byte()?;
        match byte {
            0xF8 => Some(MidiMessage::Clock),
            0xFA => Some(MidiMessage::Start),
            0xFC => Some(MidiMessage::Stop),
            0xFB => Some(MidiMessage::Continue),
            0xF0..=0xF7 => self.read_sysex(byte),
            0x80..=0xEF => self.read_channel_message(byte),
            _ => None,
        }
    }

    fn read_sysex(&mut self, start: u8) -> Option<MidiMessage> {
        let mut data = heapless::Vec::<u8, 256>::new();
        data.push(start).ok()?;
        loop {
            if let Some(b) = self.read_byte() {
                if b == 0xF7 {
                    data.push(b).ok()?;
                    break;
                }
                if data.push(b).is_err() {
                    return None;
                }
            } else {
                return None;
            }
        }
        Some(MidiMessage::Sysex(data.into_vec()))
    }

    fn read_channel_message(&mut self, status: u8) -> Option<MidiMessage> {
        let channel = status & 0x0F;
        let message_type = status & 0xF0;

        match message_type {
            0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => {
                let data1 = self.read_byte()?;
                let data2 = self.read_byte()?;
                match message_type {
                    0x80 => Some(MidiMessage::NoteOff(channel, data1, data2)),
                    0x90 => Some(MidiMessage::NoteOn(channel, data1, data2)),
                    0xA0 => Some(MidiMessage::PolyPressure(channel, data1, data2)),
                    0xB0 => Some(MidiMessage::ControlChange(channel, data1, data2)),
                    0xE0 => Some(MidiMessage::PitchBend(channel, data1, data2)),
                    _ => None,
                }
            }
            0xC0 | 0xD0 => {
                let data1 = self.read_byte()?;
                match message_type {
                    0xC0 => Some(MidiMessage::ProgramChange(channel, data1)),
                    0xD0 => Some(MidiMessage::ChannelPressure(channel, data1)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn send_clock(&mut self) {
        self.write_message(&[0xF8]);
    }

    pub fn send_start(&mut self) {
        self.write_message(&[0xFA]);
    }

    pub fn send_stop(&mut self) {
        self.write_message(&[0xFC]);
    }

    pub fn send_continue(&mut self) {
        self.write_message(&[0xFB]);
    }

    pub fn send_note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        self.write_message(&[0x90 | (channel & 0x0F), note & 0x7F, velocity & 0x7F]);
    }

    pub fn send_control_change(&mut self, channel: u8, cc: u8, value: u8) {
        self.write_message(&[0xB0 | (channel & 0x0F), cc & 0x7F, value & 0x7F]);
    }

    pub fn send_beat(&mut self, channel: u8, beat_note: u8, velocity: u8) {
        self.send_note_on(channel, beat_note, velocity);
    }

    pub fn send_downbeat(&mut self, channel: u8, downbeat_note: u8, velocity: u8) {
        self.send_note_on(channel, downbeat_note, velocity);
    }

    pub fn send_cc(&mut self, channel: u8, cc: u8, value: u8) {
        self.send_control_change(channel, cc, value);
    }
}

#[derive(Debug)]
pub enum MidiMessage {
    Clock,
    Start,
    Stop,
    Continue,
    NoteOn(u8, u8, u8),
    NoteOff(u8, u8, u8),
    ControlChange(u8, u8, u8),
    ProgramChange(u8, u8),
    PolyPressure(u8, u8, u8),
    ChannelPressure(u8, u8),
    PitchBend(u8, u8, u8),
    Sysex(heapless::Vec<u8>),
}
