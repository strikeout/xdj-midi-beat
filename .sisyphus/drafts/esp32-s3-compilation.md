# Draft: ESP32-S3 Compilation Analysis

## Current State Analysis

### Project Structure
- Workspace with 3 members: `host`, `esp32`, `esp32-emulator`
- ESP32 firmware in `esp32/` directory
- Current target: `riscv32imac-esp-espidf` (ESP32-C3/RISC-V)
- Existing CI/CD: GitHub Actions with espressif/idf container

### Key Findings
1. **Architecture Mismatch**: ESP32-S3 uses Xtensa LX7 architecture, not RISC-V
2. **Current Configuration**: `.cargo/config.toml` targets RISC-V (ESP32-C3)
3. **Build System**: Uses `esp-idf-sys` and `esp-idf-hal` crates
4. **Dependencies**: Standard ESP-IDF dependencies present
5. **GPIO Usage**: MIDI on GPIO1 (RX) and GPIO3 (TX), UART0
6. **Existing CI**: Builds in `espressif/idf:latest` container with RISC-V target

### User Requirements Confirmed
1. **Full ESP32-S3 migration** (not dual-target)
2. **Development on macOS**, builds on GitHub agents
3. **WiFi AP + MIDI functionality** needed

### ESP32-S3 Requirements
- **Target**: `xtensa-esp32s3-espidf` 
- **Toolchain**: ESP-IDF v5.1+ with Xtensa support
- **Build flags**: ESP32-S3 specific features (USB OTG, PSRAM, etc.)
- **Pin mapping**: ESP32-S3 has different peripheral mappings

## Technical Decisions Made
1. **Target**: Switch to `xtensa-esp32s3-espidf`
2. **Toolchain**: Use ESP-IDF v5.1+ with Xtensa toolchain
3. **CI/CD**: Update GitHub Actions to use Xtensa target
4. **GPIO**: Map MIDI pins appropriately for ESP32-S3
5. **Memory**: Configure PSRAM if available on target board
6. **Build Script**: Fix `esp_idf_build` import issue

## Open Questions
1. **Specific ESP32-S3 board**: Default to generic ESP32-S3
2. **PSRAM usage**: Enable if board has PSRAM
3. **USB features**: Consider USB MIDI as future enhancement
4. **WiFi antenna**: Use internal antenna by default

## Migration Strategy
1. Update target configuration
2. Fix build dependencies
3. Update CI/CD workflows
4. Test build locally
5. Update documentation