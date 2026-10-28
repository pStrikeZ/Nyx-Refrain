# AGENTS.md — Nyx Refrain

Notes for coding agents (and people) working in this repository: how it is built and checked, its conventions, and what has been learned about AirPlay 2 receivers that the code depends on. `CLAUDE.md` is a symlink to this file.

## Layout

- Binaries: `apps/nyx-refrain` (GUI: a tray flyout on Windows, an SNI tray menu on Linux) and `apps/nyxr` (CLI).
- Crates: `raop` (AirPlay 2 realtime sender: transient pair-setup, encrypted RTSP, RTP, timing, events), `pipeline` (resampling, drift compensation, packetizing), `capture` (WASAPI on Windows, PipeWire on Linux, stdin / file sources), `discovery` (mDNS), `nowplaying` (SMTC / MPRIS track info and receiver controls), `netutil`, `pwnative`.
- `fuzz/` is a separate cargo-fuzz workspace. `scripts/build.sh` builds every release artifact; `scripts/check-artifacts.sh` checks them.

## Build and checks (every change)

- Edition 2024, rustfmt 2024 style.
- **Do not run `cargo update`**: unrelated dependencies stay at their locked versions.
- Linux builds are fully static musl (`x86_64|aarch64|loongarch64-unknown-linux-musl`): `dlopen` is unavailable, so nothing may depend on a C library loaded at runtime. This is also why Linux has only the SNI tray menu and no egui window (and Wayland does not let a program place a popup anyway). Windows targets: `x86_64-pc-windows-gnu`, `aarch64-pc-windows-gnullvm`. The Windows GUI renders on the CPU (winit + egui + softbuffer), with no GPU or driver requirement.
- Checks, as CI runs them (`.github/workflows/ci.yml`):
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings` on the host and on `x86_64-pc-windows-gnu`, `aarch64-pc-windows-gnullvm` and `x86_64-unknown-linux-musl`. `#[cfg]` blocks often make a lint fire on one target only. For the musl target, `ring` needs a C compiler: `CC_x86_64_unknown_linux_musl=$PWD/scripts/zigcc-x86_64-musl.sh AR_x86_64_unknown_linux_musl="zig ar"`.
  - `cargo test --workspace`
  - Not in CI, but keep them passing: `cargo fmt --manifest-path fuzz/Cargo.toml --check` and `cd fuzz && cargo check`.
- Full artifacts: `CARGO_BUILD_JOBS=4 ./scripts/build.sh` (`TARGETS="win-x64 win-arm64"` builds a subset; needs cargo-zigbuild, zig, and `makensis` for the Windows installers), then `./scripts/check-artifacts.sh`. Output: `dist/win32-amd64/`, `dist/win32-aarch64/`, `dist/linux-*/`, `dist/packages/` (deb, rpm, Arch, NSIS installers), `dist/SHA256SUMS`. Release builds use fat LTO and one codegen unit: do not run two full builds at once.
- The version string (`NYX_VERSION`) is `v<version>-g<hash>[-dirty]` unless HEAD is the clean `v<version>` tag. That is expected for development builds.

## Testing without a real receiver

Never stream to a real receiver from tests. Use these instead:

- `crates/raop/tests/ap2_fake_receiver.rs`: an AirPlay 2 fake receiver (pair-setup, RTSP, events) that the sender's session runs against.
- A Linux virtual sink that does not become the default output: `PW_SINK_NAME=nyx_test cargo run -p capture --example pw_sink_smoke 12`, then play into it with `paplay --device=nyx_test`. Do not stack test streams on a sink that is in use.
- MPRIS end to end (ignored by default): `dbus-run-session -- cargo test -p nowplaying --test mpris_fake_player -- --ignored`.
- Long drift simulations (ignored by default, minutes of CPU): `cargo test --release -p pipeline --test drift_simulation_test -- --ignored`. The quick ones in `pipeline::drift` run with the normal tests.
- Other examples: `cargo run -p capture --example list_endpoints` (Windows render endpoints), `loopback_smoke`, `pw_volume_probe`; `cargo run -p raop --example ap2_tone` sends a test tone (to a receiver: only when its owner says so).

## Conventions

- **Versions are raised only when the code is frozen and the artifacts will actually be published**; feature commits and test builds keep the version. It is `[workspace.package] version` in `Cargo.toml`; the Windows VERSIONINFO, installer names and Linux package versions derive from it. When unsure whether a release is a patch or a minor one, ask the maintainer.
- Releases: pushing a `v*` tag runs `.github/workflows/build.yml`, which builds every artifact and creates a **draft** release with the packages and `SHA256SUMS`; the maintainer publishes it. A manual run of the workflow builds a branch with a hashed version.
- Commit messages wrap at about 72 columns. PR descriptions, issue comments and release notes are **not** wrapped by hand: one line per paragraph, one line per list item (GitHub wraps them).
- Publisher, Cargo `authors` and Linux package vendor: `pStrikeZ`. License: MIT.
- Not done on purpose: publishing to crates.io / cargo-binstall; a portable Windows exe (firewall rules are per path, so a copied exe loses them).

## What the code relies on (measured on HomePods)

### Now playing

- Track info goes in `SET_PARAMETER` over the HAP-encrypted RTSP connection (DMAP text, artwork), each with `RTP-Info: rtptime=`.
- **Artwork first, then text.** Text first makes the HomePod switch artwork IDs at once, and Home Assistant fetches the old image and never refreshes.
- No progress is sent: few players report a duration, and a progress bar left behind could not be cleared (zero-length progress and FLUSH both fail). `Ap2Session::send_progress` and its tests are kept.
- No placeholder progress before RECORD (`Ap2Config::initial_progress` is false), else players without a duration show a one-hour bar.
- Placeholder info: title "Nyx Refrain", the computer's name as artist, `assets/artwork/placeholder-cover.jpg` (a different JPEG comment each time).
- Artwork is read up to 16 MiB; anything over 1000×1000 or 1 MiB is scaled to a 1000 px JPEG.
- Control requests only time out their wait for a response; a request is always written whole (half a HAP frame ruins the session), and late responses are dropped by CSeq.

### Audio format

- The realtime `audioStream` accepts only 44.1 kHz (PCM / ALAC 16 bit, AAC-LC, AAC-ELD); 24 bit and 48 kHz exist only for buffered audio (seconds of latency). So everything is resampled to 44.1 kHz and sent as `audioFormat 0x800` (PCM/44100/16/2).
- The resampler is rubato's sinc (`SincInterpolationParameters::default()`: 256 taps). 48 → 44.1 kHz: flat to 20 kHz, aliasing −167 dB, 117 frames of delay, under 1% of one core. Cubic interpolation lost 4 dB at 20 kHz and let 23 kHz fold back at −6 dB. Tests in `crates/pipeline/src/resample.rs`.
- Sources paced by back-pressure (stdin, files) have no drift to correct: the resampler stays at 1.0, which at 44.1 kHz is a passthrough, so 16-bit input arrives bit for bit.

### Receiver controls

- The HomePod sends its own and Home Assistant's play controls over the **AP2 event channel** (`POST /command`, bplist `type: sendMediaRemoteCommand`, a four-letter `value` and a `modernMediaRemoteCommand` number), not DACP. Known: `play`/0, `paus`/1, `nitm`/4, `pitm`/5; `dvlc` reports the receiver's volume (0–1, linear to 0–100%). Seeking is not supported on purpose; unknown commands are logged at info.
- The receiver does not know the sender paused (realtime does not support `SETRATEANCHORTIME`), so "pause" from the receiver toggles by the player's actual state.
- Debugging: `RUST_LOG=info,raop::ap2::events=debug`. On Windows, do not pipe `raop::ap2::session=debug` output to `findstr` (binary dumps crash it).

### Capture on Windows

- Device (endpoint) loopback is taken after the output device's effects and volume, and before its mute: laptop speaker enhancements end up in the stream and its level follows the volume. Per-process loopback (`nyxr --source process`, the GUI default) is taken before both.
