# Multi-architecture refactor migration plan

## Objective

Restructure the workspace so that protocol logic, shared types, and cross-target behavior live in explicit shared crates, while host-only and ESP32-only concerns stay behind target-specific adapters.

Primary outcomes:
- eliminate duplicated Pro DJ Link protocol logic between `host/` and `esp32/`
- reduce coupling in `host/src/main.rs` and `host/src/state.rs`
- make emulator sharing explicit instead of routing through host exports
- simplify adding future targets and CI build matrix entries

## Current hotspots

### Shared logic currently duplicated or weakly shared
- `host/src/prolink/mod.rs`
- `host/src/prolink/packets.rs`
- `host/src/prolink/builder.rs`
- `esp32/src/prolink.rs`
- `esp32-emulator/src/main.rs`
- `esp32-emulator/src/verify_stopped.rs`

### High-coupling host files
- `host/src/main.rs`
- `host/src/state.rs`

### Target-specific code that should remain target-specific
- `host/src/link/mod.rs`
- `host/src/tui/*`
- `host/src/midi/*` transport bindings using `midir`
- `esp32/src/main.rs`
- `esp32/src/midi.rs`
- `esp32/src/webui.rs`
- `esp32/build.rs`

### Build/CI duplication
- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `.github/workflows/esp32-ci.yml`
- `.github/workflows/esp32-release.yml`
- `.github/vendor-patches/build.rs`
- `.github/vendor-patches/link_bindings.rs`

---

## Target workspace layout

```text
Cargo.toml
crates/
  xdj-core-prolink/
    Cargo.toml
    src/
      lib.rs
      constants.rs
      types.rs
      parse.rs
      build.rs
      math.rs
  xdj-core-types/
    Cargo.toml
    src/
      lib.rs
      config.rs
      ids.rs
      timing.rs
host/
esp32/
esp32-emulator/
```

Notes:
- `xdj-core-prolink` should be `no_std`-friendly if feasible.
- `xdj-core-types` is optional in the first PR; only extract it once the protocol crate is stable.
- Host remains the integration app, not the source of truth for protocol definitions.

---

## Execution strategy

Use small PRs. Do not combine protocol extraction, state decomposition, and CI cleanup in one change.

Recommended PR sequence:
1. Baseline tests and coverage notes
2. Introduce `xdj-core-prolink`
3. Migrate emulator to `xdj-core-prolink`
4. Migrate ESP32 to `xdj-core-prolink`
5. Thin host bootstrap
6. Split host state modules
7. Introduce internal adapter seams
8. Extract shared config/types
9. Move ESP32 web assets out of Rust source
10. Deduplicate CI workflows

---

## PR 1 - Baseline safety net

### Goals
- preserve behavior before structural changes
- identify minimum tests needed to refactor safely

### File actions
- inspect existing tests in `host/`, `esp32/`, `esp32-emulator/`
- add protocol-focused tests near current host protocol code if coverage is thin:
  - `host/src/prolink/packets.rs`
  - `host/src/prolink/builder.rs`
- add state behavior tests for:
  - master selection
  - BPM smoothing
  - play-state transitions

### Deliverables
- passing host tests
- passing desktop build
- passing emulator build
- passing ESP32 build in CI

### QA scenario
- Run: `cargo test -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-esp32-emulator`
- Run in CI or ESP-IDF environment: `cargo build -p xdj-clock-esp32 --release`
- Expected result:
  - all commands exit 0
  - baseline behavior is recorded before any extraction PR starts
  - any missing test coverage is identified and added before PR 2 merges

### Acceptance criteria
- every later PR can compare against the same protocol and state behavior

---

## PR 2 - Create `crates/xdj-core-prolink`

### New files
- `crates/xdj-core-prolink/Cargo.toml`
- `crates/xdj-core-prolink/src/lib.rs`
- `crates/xdj-core-prolink/src/constants.rs`
- `crates/xdj-core-prolink/src/types.rs`
- `crates/xdj-core-prolink/src/parse.rs`
- `crates/xdj-core-prolink/src/build.rs`
- `crates/xdj-core-prolink/src/math.rs`

### Move/extract from host
Source files:
- `host/src/prolink/mod.rs`
- `host/src/prolink/packets.rs`
- `host/src/prolink/builder.rs`

Extract into shared crate:
- protocol constants (`MAGIC`, `PORT_*`, `PKT_*`, device constants)
- protocol math helpers (`bpm_from_raw`, `pitch_to_percent`, `effective_bpm`)
- packet structs (`KeepAlive`, `BeatPacket`, `AbsPositionPacket`, `CdjStatus`, `MixerStatus`, `PlayState`)
- parsing functions
- packet builder functions

### Keep in host for now
- network listener tasks:
  - `host/src/prolink/discovery.rs`
  - `host/src/prolink/beat_listener.rs`
  - `host/src/prolink/status_listener.rs`
  - `host/src/prolink/virtual_cdj.rs`
  - `host/src/prolink/metadata.rs`

### Update files
- `Cargo.toml` workspace members
- `host/Cargo.toml` add path dependency on `xdj-core-prolink`
- `host/src/prolink/mod.rs` becomes thin re-export layer or task-only module

### Acceptance criteria
- host builds using shared protocol crate
- protocol tests move or pass unchanged
- no duplicated constant definitions remain in host after extraction

### QA scenario
- Run: `cargo test -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-esp32-emulator`
- Search repo for old duplicate protocol definitions and verify they are removed or reduced to re-exports:
  - `MAGIC`
  - `PORT_DISCOVERY`
  - `PKT_BEAT`
  - duplicated packet structs
- Expected result:
  - host compiles against `xdj-core-prolink`
  - protocol tests still pass
  - host is no longer the source of truth for shared protocol definitions

---

## PR 3 - Move emulator off host protocol exports

### Update files
- `esp32-emulator/Cargo.toml`
- `esp32-emulator/src/main.rs`
- `esp32-emulator/src/verify_stopped.rs`

### Changes
- replace `xdj_clock_host::prolink::*` imports with `xdj_core_prolink::*`
- keep emulator behavior the same
- stop using host crate as protocol carrier

### Why this PR is isolated
- proves the new crate works for a second consumer before touching ESP32 firmware

### Acceptance criteria
- emulator builds and runs against the shared protocol crate
- verify-stopped binary compiles unchanged in behavior

### QA scenario
- Run: `cargo build -p xdj-clock-esp32-emulator`
- Run: `cargo run -p xdj-clock-esp32-emulator`
- In a second shell, run: `cargo run -p xdj-clock-esp32-emulator --bin verify-stopped`
- Verify emulator startup and protocol simulation still work:
  - dashboard starts
  - simulator process compiles and sends packets
  - emulator logs/responds without protocol import errors
- Expected result:
  - emulator uses `xdj-core-prolink` imports only for protocol definitions
  - runtime behavior matches pre-refactor behavior

---

## PR 4 - Migrate ESP32 protocol usage

### Update files
- `esp32/Cargo.toml`
- `esp32/src/prolink.rs`
- `esp32/src/main.rs`

### Changes
- replace local protocol constants and packet structs with `xdj-core-prolink`
- keep only ESP32-specific transport and stack wiring inside `esp32/src/prolink.rs`
- if useful, split `esp32/src/prolink.rs` into:
  - `esp32/src/prolink/mod.rs`
  - `esp32/src/prolink/stack.rs`
  - `esp32/src/prolink/io.rs`

### Important constraint
- do not force host socket abstractions into ESP32 yet
- keep this PR limited to shared parsing/types/constants/builders

### Acceptance criteria
- ESP32 compiles using shared protocol definitions
- duplicated protocol structs/constants are removed from ESP32 code

### QA scenario
- Run in ESP-IDF environment: `cargo build -p xdj-clock-esp32 --release`
- Run: `cargo build -p xdj-clock-esp32-emulator`
- Verify the shared crate does not introduce host-only dependencies into ESP32 build graph
- Search `esp32/src/prolink.rs` for removed duplicated protocol constants and packet structs
- Expected result:
  - ESP32 firmware compiles successfully
  - emulator still compiles against the same shared protocol crate
  - shared protocol code remains portable across desktop and ESP32 targets

---

## PR 5 - Thin host bootstrap and runtime wiring

### Current problem
`host/src/main.rs` is doing CLI, config, interface detection, runtime composition, and policy decisions in one place.

### New files
- `host/src/app.rs`
- `host/src/runtime/mod.rs`
- optional:
  - `host/src/runtime/prolink.rs`
  - `host/src/runtime/midi.rs`
  - `host/src/runtime/link.rs`
  - `host/src/runtime/ui.rs`

### Update files
- `host/src/main.rs`
- `host/src/lib.rs`

### Changes
- `main.rs` keeps:
  - CLI parsing
  - config path selection
  - top-level error handling
- `app.rs` or `runtime/*` takes over:
  - shared state creation
  - service startup
  - task orchestration
  - headless vs TUI branching

### Acceptance criteria
- `main.rs` becomes small and readable
- startup order is explicit and testable

### QA scenario
- Run: `cargo test -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-host`
- Run host in headless mode if supported by current CLI, for example: `cargo run -p xdj-clock-host -- --no-tui --help`
- Inspect `host/src/main.rs` and confirm orchestration logic moved into `app.rs`/`runtime/*`
- Expected result:
  - no behavior regression in startup path
  - `main.rs` is mostly bootstrap and error handling

---

## PR 6 - Split `host/src/state.rs`

### New module layout
- `host/src/state/mod.rs`
- `host/src/state/device.rs`
- `host/src/state/master.rs`
- `host/src/state/metadata.rs`
- `host/src/state/phrase.rs`
- `host/src/state/smoothing.rs`

### Extraction map
From `host/src/state.rs`:
- per-device storage and lifecycle -> `device.rs`
- master state selection and derived view -> `master.rs`
- track metadata structs and update flow -> `metadata.rs`
- phrase/song structure types -> `phrase.rs`
- BPM smoothing logic -> `smoothing.rs`

### Keep stable initially
- preserve the external `SharedState` interface as much as possible
- change internal organization first, public behavior second

### Acceptance criteria
- smaller, targeted tests for state behavior
- no change in runtime behavior

### QA scenario
- Run: `cargo test -p xdj-clock-host`
- Add or run focused tests for:
  - master selection
  - BPM smoothing
  - track metadata attachment
  - phrase state transitions
- Expected result:
  - state behavior remains stable
  - module split does not change externally observed master/state decisions

---

## PR 7 - Introduce internal adapter seams

### Goal
Separate reusable logic from target-specific I/O.

### Candidate internal traits
- `MidiTransport`
- `ClockScheduler`
- `DiscoverySocket` or `PacketSource`

### Suggested host files
- `host/src/midi/transport.rs`
- `host/src/net/transport.rs`

### Suggested ESP32 files
- `esp32/src/midi/transport.rs`
- `esp32/src/net/transport.rs`

### Scope control
- do not over-abstract everything
- only introduce traits where host and ESP32 already implement the same behavior differently

### First adapter targets
1. MIDI send interface
2. clock scheduling surface
3. packet input/output surface

### Acceptance criteria
- shared logic can depend on an interface instead of `midir` or ESP-IDF directly

### QA scenario
- Run: `cargo build -p xdj-clock-host`
- Run in ESP-IDF environment: `cargo build -p xdj-clock-esp32 --release`
- Verify adapter traits are consumed by shared logic while transport crates remain target-local
- Expected result:
  - shared code compiles without directly importing target-only transport dependencies
  - host and ESP32 still compile with their own transport implementations

---

## PR 8 - Extract shared config/types

### New files
- `crates/xdj-core-types/Cargo.toml`
- `crates/xdj-core-types/src/lib.rs`
- `crates/xdj-core-types/src/config.rs`
- `crates/xdj-core-types/src/ids.rs`
- `crates/xdj-core-types/src/timing.rs`

### Candidate moves from host
From `host/src/config.rs`:
- source selection enum
- MIDI mapping shapes
- common timing config pieces

### Consumers
- `host`
- `esp32-emulator`
- maybe `esp32`, but only for truly shared types

### Important constraint
- do not move host-only CLI concerns into shared types
- do not move ESP32 AP/network settings unless another target needs them

### Acceptance criteria
- emulator and host stop drifting on equivalent config structures

### QA scenario
- Run: `cargo build -p xdj-clock-host`
- Run: `cargo build -p xdj-clock-esp32-emulator`
- Run: `cargo test -p xdj-clock-host`
- Verify shared config enums/types are imported from `xdj-core-types` where intended
- Expected result:
  - host and emulator use the same shared type definitions for shared concerns
  - target-specific config remains local

---

## PR 9 - Move ESP32 web assets out of Rust source

### Current hotspot
- `esp32/src/webui.rs` embeds a large HTML/CSS/JS string

### New files
- `esp32/webui/index.html`
- `esp32/webui/app.js`
- `esp32/webui/styles.css`

### Update files
- `esp32/src/webui.rs`
- optional `esp32/build.rs` if asset preprocessing is needed

### Approach
- use `include_str!` for a minimal first step
- keep server handlers in Rust, move assets out

### Acceptance criteria
- no giant inline asset blob in Rust source
- UI assets can be edited independently

### QA scenario
- Run: `cargo build -p xdj-clock-esp32`
- Serve or run the ESP32 UI path in the current target flow
- Open the dashboard with a browser-based check or HTTP fetch against the served route
- Verify:
  - HTML is served successfully
  - JS/CSS assets load successfully
  - no inline Rust string blob remains as the source of truth
- Expected result:
  - asset extraction does not change served UI behavior
  - frontend assets are independently editable files

---

## PR 10 - Deduplicate CI and release setup

### Current duplication
- Rust vendoring and patching repeated in:
  - `.github/workflows/ci.yml`
  - `.github/workflows/release.yml`
- ESP32 toolchain/container setup repeated in:
  - `.github/workflows/esp32-ci.yml`
  - `.github/workflows/esp32-release.yml`

### New files
- `.github/actions/setup-rust-vendor/action.yml`
- `.github/actions/setup-esp32/action.yml`
- or reusable workflow equivalents

### Changes
- move common vendored `rusty_link` restore/download/patch logic into one reusable action
- move ESP32 environment bootstrap into one reusable action
- centralize target matrix definitions where possible

### Acceptance criteria
- workflow changes for new targets happen in one place
- less copy/paste between CI and release flows

### QA scenario
- Validate workflow syntax after extraction
- Trigger or dry-run CI on a branch touching shared workflow setup
- Verify both desktop and ESP32 pipelines still perform:
  - vendor restore/patching
  - toolchain setup
  - build steps
  - artifact generation
- Expected result:
  - workflows remain green
  - common setup changes are made in one reusable definition instead of duplicated blocks

---

## File-by-file migration checklist

### Root
- `Cargo.toml`
  - add new workspace members
  - keep profile settings intact

### Host
- `host/Cargo.toml`
  - add path deps to new crates
- `host/src/main.rs`
  - reduce to bootstrap
- `host/src/lib.rs`
  - export app/runtime/state modules cleanly
- `host/src/prolink/mod.rs`
  - retain listener/task glue only
- `host/src/prolink/packets.rs`
  - move to shared crate, then delete or thin wrapper
- `host/src/prolink/builder.rs`
  - move to shared crate, then delete or thin wrapper
- `host/src/state.rs`
  - split into `state/` module tree

### ESP32
- `esp32/Cargo.toml`
  - add shared crate dependency
- `esp32/src/prolink.rs`
  - strip shared protocol definitions, keep target-specific stack logic
- `esp32/src/main.rs`
  - adapt imports only as needed
- `esp32/src/webui.rs`
  - convert to asset-serving module

### Emulator
- `esp32-emulator/Cargo.toml`
  - add direct dep on shared protocol crate
- `esp32-emulator/src/main.rs`
  - switch imports from host protocol exports
- `esp32-emulator/src/verify_stopped.rs`
  - switch imports from host protocol exports

### GitHub Actions
- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `.github/workflows/esp32-ci.yml`
- `.github/workflows/esp32-release.yml`
  - replace repeated setup steps with shared actions

---

## Verification plan per PR

For every PR:
1. run relevant tests for touched crate(s)
2. run workspace build if dependency graph changed
3. run emulator build when protocol or shared types move
4. run ESP32 build when `esp32/` or shared protocol crate changes
5. verify no duplicated constants or packet structs remain in old locations after migration
6. verify the PR-specific QA scenario above before merge, not just generic compile success

---

## Rollback rules

- If `xdj-core-prolink` extraction becomes too broad, stop after constants + parsers and defer builders to the next PR.
- If `state.rs` split causes behavior churn, preserve public types and only move internals first.
- If adapter traits start multiplying without simplifying code, narrow the seam to MIDI first and defer network abstraction.

---

## Success definition

The migration is complete when:
- protocol logic has a single source of truth
- emulator depends on shared core crates directly
- ESP32 shares protocol definitions without inheriting host runtime concerns
- host bootstrap is thin and state logic is decomposed
- CI setup is centralized enough that new targets do not require copy/paste workflow changes

## First implementation slice to execute

If starting immediately, do this exact order:
1. PR 1 baseline tests
2. PR 2 create `xdj-core-prolink`
3. PR 3 migrate emulator
4. PR 4 migrate ESP32

That gets the highest-value multi-architecture sharing improvement before touching host state or runtime structure.
