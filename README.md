# xdj-midi

[![Version](https://img.shields.io/badge/version-1.1.7-blue.svg)](Cargo.toml)

A robust bridge between Pioneer Pro DJ Link (CDJ/XDJ), Ableton Link, and MIDI. Synchronize your DAWs, drum machines, and hardware synths with your DJ setup with sub-millisecond precision.

---

## 🏗 Architecture

The project is structured as a Rust workspace with shared core logic to ensure consistency across desktop and embedded platforms.

- **`host/`**: Shared core library and Desktop/Laptop application.
  - **Shared Library**: Centralized Pro DJ Link packet parsing/building and global state management.
  - **TUI Dashboard**: Real-time Terminal UI with device discovery, BPM status, and interactive MIDI port selection.
  - **Ableton Link**: Integrated Link engine with hybrid "Auto" mode priority.
- **`esp32/`**: Firmware for embedded ESP32 hardware.
  - **WiFi AP**: Creates a dedicated `xdj-midi-setup` network.
  - **GPIO MIDI**: Low-latency hardware MIDI IN/OUT via UART.
- **`esp32-emulator/`**: High-performance native emulator of the ESP32 firmware.
  - **Web Dashboard**: Modern, responsive CSS Grid interface with real-time WebSocket updates.
  - **Verification Tools**: Built-in simulators for testing master handoff and stopped-deck scenarios.

---

## ✨ Features

### Pro DJ Link (Ethernet)
- **Proactive Participation**: Actively joins the ProLink network as a Virtual CDJ (Device #5 default), triggering real hardware to send detailed status updates.
- **Modern Hardware Support**: Full support for CDJ-3000 and XDJ-AZ absolute position packets and metadata.
- **Hierarchical Master Tracking**: Intelligent tempo master selection with explicit master flags and playing-state fallbacks.

### Ableton Link
- **Hybrid "Auto" Mode**: Prioritizes Ableton Link peers when present, falling back to Pro DJ Link hardware automatically when the Link session is empty.
- **Sub-millisecond Polling**: 500µs polling interval for extremely tight MIDI clock synchronization.
- **Peer Visibility**: Real-time peer count and tempo synchronization visible in the TUI and logs.

### MIDI Output
- **24-PPQ Clock**: Stable MIDI Clock (0xF8) pulses with Start/Stop/Continue support.
- **Rich CC Mapping**: Remappable CCs for BPM (coarse/fine), Pitch (-10%/+10%), Bar Phase, Beat Phase, and Playing state.
- **Note Triggers**: MIDI Note triggers on every beat and downbeat with velocity accents.

---

## 🚀 Quick Start

### Building the Host (Desktop)
1.  **Vendor Setup**: Ableton Link support requires vendoring `rusty_link`.
    ```sh
    mkdir -p vendor
    curl -sL https://crates.io/api/v1/crates/rusty_link/0.2.3/download | tar xzf - -C vendor
    mv vendor/rusty_link-0.2.3 vendor/rusty_link
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

Settings are managed via `config.toml` for the host and `sdkconfig` for the ESP32.

```toml
# config.toml defaults
source = "auto"       # "auto" | "link" | "prolink"
device_number = 5     # Virtual CDJ ID
interface = "auto"    # Network adapter selection

[midi]
output = "auto"       # MIDI port substring match
clock_enabled = true
clock_loop_enabled = true
smoothing_ms = 30     # BPM jitter reduction
```

---

## 🛠 Project State (v1.1.7)
- **v1.1.0**: Migrated emulator to WebSockets.
- **v1.1.2**: Implemented proactive Pro DJ Link participation (Keep-alives + Unicasts).
- **v1.1.4**: Fixed master handoff and play-state latching/timeouts.
- **v1.1.7**: Restored Ableton Link with critical dangling-pointer fixes in `rusty_link`.

---

## 📜 License
**Proprietary** — All rights reserved. Uses components licensed under MIT and GPL-2.0-or-later (Ableton Link).
