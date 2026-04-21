# xdj-midi

[![Version](https://img.shields.io/badge/version-1.2.9-blue.svg)](Cargo.toml)



![Terminal UI in action](./docs/tui_v1.2.9.png?auto=compress&cs=tinysrgb&dpr=3 "TUI on MacOS")


---

### Bridge between Pioneer CDJ/XDJ hardware (Pro DJ Link), Ableton Link and MIDI clock, MTC, CC, and note output.



## ✨ Features

### Pro DJ Link (Ethernet)
- Joins the ProLink network as a Virtual CDJ (device #5 by default).
- Supports CDJ-3000 and XDJ-AZ absolute-position packets and metadata.
- Tracks tempo master with explicit master flags and playing-state fallback.

### Ableton Link
- Auto mode prefers Link peers when present and falls back to Pro DJ Link.
- Default polling is 500µs for tight MIDI clock synchronization.
- Peer count and tempo are visible in the TUI and logs.

### MIDI Output
- Stable MIDI Clock (0xF8) pulses with Start/Stop/Continue support.
- Remappable CCs for BPM, pitch, bar phase, beat phase, playing state, master deck, and phrase-16.
- MIDI notes on every beat and on each downbeat.

---

## 🚀 Quick Start

### Download the release
1. download the latest release on the right
2. run it.
---

## Slow  Start 
### Compile the Host (Desktop)
1.  **Vendor Setup**: Ableton Link support requires vendoring `rusty_link`.
    ```sh
    mkdir -p vendor
    curl -sL https://crates.io/api/v1/crates/rusty_link/0.2.3/download | tar xzf - -C vendor
    mv vendor/rusty_link-0.2.3 vendor/rusty_link
    cp .github/vendor-patches/build.rs vendor/rusty_link/build.rs
    cp .github/vendor-patches/link_bindings.rs vendor/rusty_link/link_bindings.rs
    ```
2.  **Run**:
    ```sh
    cargo run -p xdj-clock-host
    ```

### Building the ESP32 Firmware
```sh
. /path/to/esp-idf/export.sh
cargo build --release -p xdj-clock-esp32
```

### Running the Emulator
```sh
cargo run -p xdj-clock-esp32-emulator
```
Open `http://localhost:8080` to view the responsive WebSocket dashboard.

---

## ⚙️ Configuration

Settings are managed via `config.toml` for the host and `sdkconfig` for the ESP32. You should be able to just run the app without any changes to these just fine.

```toml
# config.toml defaults
source = "auto"       # "auto" | "link" | "prolink"
device_number = 5      # Virtual CDJ ID
interface = "auto"    # Network adapter selection
device_name = "xdj-clock"

[midi]
output = "auto"       # MIDI port substring match
clock_enabled = true
clock_loop_enabled = true
smoothing_ms = 0       # BPM jitter reduction
latency_compensation_ms = 0
phrase_lock_stable_beats = 4

[midi.notes]
channel = 9
beat = 36
downbeat = 37
phrase_change = 38

[midi.cc]
channel = 0
bpm_coarse = 1
bpm_fine = 33
pitch = 2
bar_phase = 3
beat_phase = 4
playing = 5
master_deck = 6
phrase_16 = 7

[midi.mtc]
enabled = false
frame_rate = "25"

[link]
enabled = false
quantum = 4.0
poll_interval_us = 500
```

---

## 🏗 Architecture

The project is a Rust workspace with shared core logic across desktop and embedded targets.

- **`host/`**: Desktop app and shared core library.
  - Pro DJ Link packet parsing and shared state management.
  - Real-time TUI with device discovery, BPM/phase, and MIDI port selection.
  - Ableton Link engine with hybrid auto mode.
- **`esp32/`**: ESP32 firmware.
  - WiFi AP setup.
  - UART MIDI in/out.
- **`esp32-emulator/`**: Native emulator for the ESP32 firmware.
  - Web dashboard with WebSocket updates.
  - Simulators for master handoff and stopped-deck cases.

Core outputs:
- **24-PPQ MIDI clock** — Start / Stop / Continue + 0xF8 pulses from DJ tempo
- **MIDI CC** — BPM coarse/fine, pitch, bar phase, beat phase, playing state, master deck, phrase-16
- **MIDI notes** — beat trigger and downbeat trigger with velocity accents
- **Source modes** — Pro DJ Link, Ableton Link, or auto
- **Network auto-discovery** — picks a suitable interface automatically
- **TOML config** — remappable MIDI channel, note, and CC numbers
- **CLI overrides** — interface, MIDI port, source, device number, log level
- **TUI** — live status, port selection, and logging via [ratatui](https://ratatui.rs)

---

## 🛠 Project State (v1.2.9)
- v1.1.0: Migrated emulator to WebSockets.
- v1.1.2: Added proactive Pro DJ Link participation.
- v1.1.4: Fixed master handoff and play-state latching.
- v1.1.7: Restored Ableton Link after `rusty_link` fixes.
- v1.2.9: Tightened MIDI clock/MTC scheduling and pitch-correct beat timing.

---

## 📜 License
All rights reserved. Uses components licensed under MIT and GPL-2.0-or-later (Ableton Link).
