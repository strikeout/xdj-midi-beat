# xdj-clock

[![CI](https://github.com/strikeout/xdj-midi-beat/actions/workflows/ci.yml/badge.svg)](https://github.com/strikeout/xdj-midi-beat/actions/workflows/ci.yml)
[![Release](https://github.com/strikeout/xdj-midi-beat/actions/workflows/release.yml/badge.svg)](https://github.com/strikeout/xdj-midi-beat/actions/workflows/release.yml)
[![GitHub stars](https://img.shields.io/github/stars/strikeout/xdj-midi-beat?style=flat-square)](https://github.com/strikeout/xdj-midi-beat/stargazers)
[![GitHub release (latest by date)](https://img.shields.io/github/v/release/strikeout/xdj-midi-beat?style=flat-square)](https://github.com/strikeout/xdj-midi-beat/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](https://opensource.org/licenses/MIT)

Bridge between Pioneer CDJ/XDJ hardware (Pro DJ Link) or rekordbox Performance mode (Ableton Link) and MIDI clock, CC, and note output. Lets any DAW, drum machine, or hardware synth stay in sync with your DJ setup.

---

## Features

- **24-PPQ MIDI clock** — tight Start / Stop / Clock (0xF8) pulses derived from the DJ master tempo
- **MIDI CC** — continuous BPM coarse/fine, pitch, bar phase, beat phase, playing state, master deck number
- **MIDI notes** — beat trigger (every beat) and downbeat trigger (beat 1 of every bar), with velocity accents
- **Two source modes:**
  - **Pro DJ Link** — listens on the Ethernet network for CDJ-3000 / XDJ-AZ / XDJ-XZ absolute-position packets (~30 ms) and beat packets; works with any Pioneer standalone hardware
  - **Ableton Link** — joins the Link session broadcast by rekordbox (Performance mode / USB controller) or any other Link-capable software; polls at ~500 µs for sub-millisecond timing
  - **Auto** — prefers Ableton Link when peers are detected, falls back to Pro DJ Link
- **TOML config file** — all MIDI channel, note, and CC numbers are fully remappable
- **CLI overrides** — interface, MIDI port, source, device number, log level
- **Terminal UI (TUI)** — real-time dashboard with input devices, BPM/phase, MIDI output status, interactive port selection, and a scrolling log panel (powered by [ratatui](https://ratatui.rs))

---

## Requirements

| Mode | What you need |
|---|---|
| Pro DJ Link | CDJ-3000 / XDJ-AZ / XDJ-XZ connected to the same Ethernet switch as your computer |
| Ableton Link | rekordbox 6+ in Performance mode (USB controller), or any Ableton Link peer on the same network |

**MIDI output:** any virtual or physical MIDI port visible to your OS (e.g. `loopMIDI` on Windows, IAC Driver on macOS).

---

## Installation

Download the latest binary for your platform from the [Releases](https://github.com/strikeout/xdj-midi-beat/releases) page:

| Platform | File |
|---|---|
| Windows x86-64 | `xdj-clock-windows-x86_64.zip` |
| Linux x86-64 | `xdj-clock-linux-x86_64.tar.gz` |
| macOS aarch64 (Apple Silicon) | `xdj-clock-macos-aarch64.tar.gz` |

Extract the archive — it contains the `xdj-clock` binary and a default `config.toml`.

---

## Quick start

```sh
# Auto mode: use Ableton Link if peers present, else Pro DJ Link
xdj-clock

# Force Pro DJ Link only
xdj-clock --source prolink

# Force Ableton Link only (rekordbox Performance mode)
xdj-clock --source link

# Choose a specific MIDI output port
xdj-clock --midi "loopMIDI Port"

# List available MIDI ports
xdj-clock --list-midi

# Use a custom config file
xdj-clock --config /path/to/my-config.toml

# Disable the TUI (headless / log-only mode)
xdj-clock --no-tui
```

---

## Terminal UI

By default xdj-clock launches a full-screen terminal dashboard with six panels:

```
┌─ Header ─────────────────────────────────────────┐
│ xdj-clock  │ source: Auto  │ iface: 192.168.1.5 │
├─ Left Column ──────────┬─ Right Column ──────────┤
│ ┌─ Input ────────────┐ │ ┌─ BPM ──────────────┐ │
│ │ Devices on network │ │ │ 128.00 BPM         │ │
│ │ Link peers: 2      │ │ │ +0.50%  ▮▯▯▯       │ │
│ └────────────────────┘ │ │ beat 2/4  playing   │ │
│ ┌─ MIDI Out ─────────┐ │ └────────────────────┘ │
│ │ Port: IAC Bus 1    │ │ ┌─ Output ───────────┐ │
│ │ > IAC Bus 1   [*]  │ │ │ Clock: 24 ppq ✓    │ │
│ │   IAC Bus 2        │ │ │ CCs: bpm=1 pitch=2 │ │
│ └────────────────────┘ │ │ Notes: beat=36      │ │
│                        │ └────────────────────┘ │
├─ Log ────────────────────────────────────────────┤
│ INFO xdj-clock starting ...                      │
│ INFO Device appeared CDJ-3000 ...                │
└──────────────────────────────────────────────────┘
```

### Keyboard shortcuts

| Key | Action |
|---|---|
| `q` / `Ctrl+C` | Quit |
| `Tab` | Focus the MIDI Out port selector |
| `↑` / `↓` | Navigate the port list |
| `Enter` | Connect to the selected MIDI port |
| `r` | Refresh the list of available MIDI ports |

Pass `--no-tui` to disable the dashboard and run in headless mode with plain log output to stderr (useful for background/service deployments).

---

## Configuration

All settings live in `config.toml` (looked up next to the binary, or set via `--config`).

```toml
# Beat/tempo source: "auto" | "link" | "prolink"
source = "auto"

# Network interface for Pro DJ Link ("auto" = first non-loopback IPv4)
interface = "auto"

# Virtual CDJ device number advertised on the Pro DJ Link network (1-15)
# Use 7+ to observe without conflicting with real hardware
device_number = 7

# Name broadcast on the network (max 16 bytes ASCII)
device_name = "xdj-clock"

[link]
enabled = true        # participate in the Ableton Link session
quantum = 4.0         # beats per bar / loop cycle
poll_interval_us = 500  # Link timeline poll rate in microseconds

[midi]
output = "auto"       # MIDI port name (substring match) or "auto"
clock_enabled = true  # send 24-PPQ MIDI clock
smoothing_ms = 30     # BPM smoothing window (0 = off)
latency_compensation_ms = 0  # output timing offset in ms (-1000 to +1000)

[midi.notes]
channel = 9    # 0-indexed (9 = channel 10, traditional drum channel)
beat = 36      # note fired on every beat (C1 = kick in GM)
downbeat = 37  # note fired on beat 1 only (C#1)

[midi.cc]
channel = 0     # 0-indexed (0 = channel 1)
bpm_coarse = 1  # integer BPM mapped to 0-127 (range 60-187)
bpm_fine = 33   # fractional BPM (.00 part) mapped to 0-127
pitch = 2       # pitch fader -10%..+10% → 0..127 (centre = 64)
bar_phase = 3   # position within bar 0.0-1.0 → 0-127
beat_phase = 4  # position within beat 0.0-1.0 → 0-127
playing = 5     # transport state: 0 = stopped, 127 = playing
master_deck = 6 # device number of tempo master (0 = none)
phrase_16 = 7   # fires on every 16-beat downbeat (value 0)
```

### MIDI beat velocity

| Beat position | Velocity |
|---|---|
| Beat 1 (downbeat) | 127 |
| Beat 3 | 64 |
| Beats 2 & 4 | 80 |

---

## Building from source

### Prerequisites

- Rust stable toolchain (`rustup install stable`)
- CMake ≥ 3.14
- A C++14 compiler:
  - **Windows:** MinGW-w64 (`gcc-14+`) **or** MSVC (Visual Studio 2019+)
  - **Linux:** `gcc` / `clang` + `build-essential`
  - **macOS:** Xcode command-line tools

### Vendor setup

The Ableton Link wrapper (`rusty_link`) is vendored locally but **not** tracked in git. After cloning, download and extract it:

```sh
# Download the patched rusty_link crate from crates.io and extract it:
mkdir -p vendor
curl -sL https://crates.io/api/v1/crates/rusty_link/0.2.3/download | tar xzf - -C vendor
mv vendor/rusty_link-0.2.3 vendor/rusty_link
```

Or, if you have a local copy from a previous checkout, simply place it at `vendor/rusty_link/`.

> **Note:** The vendored `build.rs` uses CMake directly (no `bindgen`), so you do **not** need `libclang`. The crates.io version is patched via `[patch.crates-io]` in `Cargo.toml` to use this local copy.

### Windows (MinGW-w64)

```powershell
# Add MinGW and CMake to PATH, then:
$env:PATH = "C:\mingw64\bin;C:\cmake\bin;$env:PATH"
rustup target add x86_64-pc-windows-gnu
cargo build --release
```

### Linux / macOS

```sh
# Install cmake if needed:
#   Ubuntu/Debian: sudo apt install cmake build-essential
#   macOS:         brew install cmake

cargo build --release
```

The binary is placed at `target/release/xdj-clock` (or `xdj-clock.exe` on Windows).

---

## How it works

```
┌───────────────────────────────────────────────────────┐
│                     xdj-clock                         │
│                                                       │
│  Pro DJ Link (UDP 50001/50002)     Ableton Link       │
│  ┌─────────────────────────┐   ┌──────────────────┐  │
│  │  beat_listener          │   │  link engine      │  │
│  │  (BeatPacket /          │   │  (polls at 500µs) │  │
│  │   AbsPositionPacket)    │   └────────┬─────────┘  │
│  └───────────┬─────────────┘            │             │
│              │  BeatEvent broadcast channel           │
│              └──────────────┬───────────┘             │
│                             ▼                         │
│              ┌──────────────────────────┐             │
│              │  SharedState (MasterState)│             │
│              └──────────────────────────┘             │
│                     ▲            ▲                    │
│              ┌──────┴──┐   ┌─────┴──────┐            │
│              │  MIDI   │   │  MIDI      │            │
│              │  clock  │   │  mapper    │            │
│              │ (24PPQ) │   │ (CC+Notes) │            │
│              └─────────┘   └────────────┘            │
└───────────────────────────────────────────────────────┘
                         │
                    MIDI output port
```

- **Pro DJ Link mode:** opens a virtual CDJ on the network, receives beat/abs-position UDP packets from hardware CDJs, derives BPM and phase.
- **Ableton Link mode:** joins the Link multicast session, captures the session timeline at ~500 µs intervals, detects beat crossings, fires MIDI events.
- **Auto mode:** monitors Link peer count; if ≥ 1 Link peer is present it takes priority, otherwise falls back to Pro DJ Link.

---

## Vendor notice

This project depends on [rusty_link](https://github.com/anzbert/rusty_link) (a Rust wrapper for [Ableton Link](https://github.com/Ableton/link)), vendored locally under `vendor/rusty_link/` (not tracked in git — see [Vendor setup](#vendor-setup)). Both rusty_link and Ableton Link are licensed under **GPL-2.0-or-later**. The vendored copy has been patched to replace the `bindgen` code-generation step with hand-written FFI bindings, enabling builds without `libclang`.

---

## License

MIT for the xdj-clock application code. See `vendor/rusty_link/link/LICENSE.md` for the Ableton Link license.
