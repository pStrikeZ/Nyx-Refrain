<p align="center">
  <img src="assets/icons/hicolor/256x256/apps/nyx-refrain.png" width="200" alt="Nyx Refrain">
</p>

<h1 align="center">Nyx Refrain</h1>

<p align="center">Stream your Windows / Linux PC's system audio to HomePod and other AirPlay 2 speakers with low latency.</p>

<p align="center">English | <a href="README.zh-cn.md">简体中文</a></p>

Nyx Refrain is an AirPlay 2 realtime sender written in pure Rust. It does its own pairing, encryption, timing and retransmission, with no dependency on OpenSSL, Bonjour for Windows or any other C library. Each stream connects to one receiver; the default end-to-end latency is about 140 ms, so video and games stay roughly in sync.

> Current version: 0.9.x, tested mainly on HomePod mini (tvOS 27). Reports from other AirPlay 2 receivers are welcome.

> [!NOTE]
> **LLM Disclaimer**
>
> All code in this project is 100% AI-generated. The maintainer may keep the project usable, but cannot guarantee code quality. The maintainer may also answer user questions with 100% copy-pasted replies; this is normal, please be aware.

---

## Features

- **Low-latency streaming**: AirPlay 2 realtime mode with three latency profiles (about 97 / 137 / 237 ms). Clock drift between the PC and the speaker is compensated automatically, so long sessions stay in tune and don't drop out.
- **Windows tray app**: a Windows 11-style tray flyout, with every common action also in the right-click menu. The UI is rendered on the CPU, needs no graphics driver, works in VMs without a GPU, and uses only a few MiB of memory when idle.
- **Per-process capture (Windows)**: captures each application's audio directly by default, so laptop speaker effects don't leak into the stream and muting the local speakers doesn't silence the speaker you're streaming to. You can switch back to classic device loopback capture in the settings.
- **Linux tray and virtual sound card**: controlled from a StatusNotifierItem tray (native on KDE, an extension on GNOME). While streaming, a PipeWire virtual output named "Nyx Refrain (AirPlay)" is created and set as the default output; it is removed again when you stop.
- **Track info and artwork**: sends the title, artist, album and artwork of what's playing on your PC to the speaker (Windows SMTC / Linux MPRIS).
- **Control from the speaker**: play, pause, previous and next pressed on the HomePod or in Home Assistant are passed to the player on your PC, and volume changes made on the speaker are synced back.
- **Autostart and resume**: can start silently to the tray at login, and reconnects automatically if it was streaming when it last exited.
- **Command-line tool `nyxr`**: streaming, device discovery, network interface diagnostics and test tones, for scripts and headless setups.
- **Proxy / TUN friendly**: detects and avoids virtual adapters from Clash, sing-box, WireGuard, Tailscale and others, and always connects through the physical interface on the speaker's subnet.

---

## Installation

Windows (x64 / ARM64) and Linux (x86_64 / aarch64 / loongarch64) are treated equally: every release ships packages for all of these architectures, with the same features.

Download the package for your system and architecture from [Releases](https://github.com/pStrikeZ/Nyx-Refrain/releases).

### Windows

| Architecture | Installer |
|---|---|
| x64 (Intel / AMD) | `nyx-refrain-<version>-windows-x86_64-setup.exe` |
| ARM64 (Snapdragon etc.) | `nyx-refrain-<version>-windows-arm64-setup.exe` |

- Installs to `C:\Program Files\Nyx Refrain`, with optional Start menu / desktop shortcuts. The install directory is added to `PATH` by default, so `nyxr` works in any new terminal.
- Uninstalling also removes the autostart entry, the `PATH` entry and the firewall rules Nyx Refrain added; settings are kept in `%APPDATA%\nyx-refrain`.
- Per-process capture needs Windows 10 2004 or later; older systems fall back to device loopback capture automatically.
- Always install with the installer rather than copying the exe elsewhere: firewall rules are tied to the program path, so a moved exe has to be allowed again.

### Linux

Packages are available for x86_64, aarch64 and loongarch64. Each contains the tray app `nyx-refrain` and the command-line tool `nyxr`, both statically linked with no hard library dependencies.

```bash
sudo apt install ./nyx-refrain_*.deb          # Debian / Ubuntu
sudo dnf install ./nyx-refrain-*.rpm          # Fedora
sudo pacman -U ./nyx-refrain-*.pkg.tar.zst    # Arch Linux
```

- System audio capture needs **PipeWire** and **WirePlumber** running.
- The tray needs a desktop that supports StatusNotifierItem:
  - **KDE Plasma**: supported natively.
  - **GNOME**: install and enable the [AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/) extension (usually preinstalled on Ubuntu). If no tray is available, a desktop notification tells you so.
  - Other desktops / window managers: use a tray module with SNI support (e.g. Waybar, or Polybar with `snixembed`).
- On Linux there is only the tray menu, no window UI.

---

## Usage

### Tray app

1. Start **Nyx Refrain**. On Windows, click the tray icon to open the flyout; on Linux, right-click the tray icon to open the menu.
2. Pick a speaker on your local network. If it isn't discovered, enter its address manually (e.g. `192.0.2.10` or `192.0.2.10:7000`).
3. Click **Start Streaming**. Everything your PC plays now comes out of the speaker.

**Windows Firewall**: AirPlay 2 needs the speaker to connect back to your PC (timing and control channels), so Nyx Refrain must be allowed to accept inbound connections. On first use, if the flyout says the firewall is blocking it, click **Allow in Firewall** (this asks for administrator permission).

**Settings** (the ⚙ in the top-right corner of the flyout on Windows, the tray menu on Linux): **Language**, **Send now playing**, **Allow receiver playback controls**, **Resume streaming on launch**, **Launch at login**; Windows also has **Use device loopback capture**.

### Command line: `nyxr`

```bash
# Discover AirPlay devices on the local network
nyxr discover

# Stream this PC's system audio (device name or IP)
nyxr stream --target "Living Room" --audible
nyxr stream --target 192.0.2.10 --audible --now-playing

# Choose a latency profile and print stream status every 5 seconds
nyxr stream --target 192.0.2.10 --audible --latency-profile stable --stats

# Stream a WAV file, or any audio via standard input
nyxr stream --target 192.0.2.10 --source wav --device music.wav --audible
ffmpeg -i input.flac -f s16le -ar 44100 -ac 2 - | nyxr stream --target 192.0.2.10 --source stdin --audible

# Show how network interfaces are classified (physical / virtual)
nyxr list-interfaces
```

- `--volume`: 0 is mute, 1–100 is a percentage (mapped to −30..0 dB), or give a negative dB value directly, e.g. `-15`.
- `--source`: `wasapi` (device loopback) by default on Windows, `process` for per-process capture; `pipewire` by default on Linux. `sine`, `wav` and `stdin` are also available.
- See `nyxr --help` and `nyxr <subcommand> --help` for all options.

Common options can go in a config file, after which `nyxr stream --audible` is enough:

- Windows: `%APPDATA%\nyx-refrain\config.toml`
- Linux: `~/.config/nyx-refrain/config.toml`

```toml
target = "192.0.2.10"   # device IP or name
interface = "Wi-Fi"     # optional: network interface to use
volume = 40.0           # initial volume (percent)
log_level = "info"
```

### Latency profiles

| Profile | End-to-end latency (measured on HomePod) | Notes |
|---|---|---|
| `low` | about 97 ms | little headroom for Wi-Fi jitter |
| `balanced` (default) | about 137 ms | recommended for most setups |
| `stable` | about 237 ms | the receiver's native buffering, for poor networks |

You can also set it directly with `--ap2-sync-latency-ms` (−165 to 3000 ms).

---

## Proxies and TUN

Nyx Refrain automatically excludes TUN / TAP and other virtual adapters and only connects from the physical interface on the speaker's subnet. If your proxy software takes over local network traffic, add direct rules for your LAN and mDNS as well, for example (Clash / mihomo):

```yaml
rules:
  - IP-CIDR,192.168.0.0/16,DIRECT   # change to your LAN subnet
  - IP-CIDR,224.0.0.251/32,DIRECT   # mDNS
  - DOMAIN-SUFFIX,local,DIRECT
```

**Streaming from outside your network through a VPN / TUN is not supported.** AirPlay 2 requires the speaker to connect back to the sender directly, which proxy-style TUNs and VPNs that do address translation cannot provide. In that case the app reports **Receiver not on this network**.

---

## Known limitations

- AirPlay 2 only, no AirPlay 1; one receiver per stream, no HomePod stereo pairs or multi-room yet.
- No macOS version yet.
- Pressing pause on the speaker pauses the player on your PC, but the audio stream itself keeps running; the speaker shows no progress bar and seeking from the speaker is not supported.
- Track info from third-party senders is not shown in Apple's Home app or Control Center; tools based on pyatv, such as Home Assistant, do show it.

---

## Roadmap

Two features are planned but currently blocked on test hardware:

- **HomePod stereo pairs**: the two HomePods in a stereo pair each need their own connection on one shared playback timeline, and getting them in sync takes two real devices.
- **macOS version**: needs a Mac to develop and test system audio capture on.

If you have this hardware and would like to help test, please say so in an issue.

---

## Building from source

All targets are cross-compiled on Linux. You need:

- Rust (stable; CI uses 1.95.0) with the targets `x86_64-pc-windows-gnu`, `aarch64-pc-windows-gnullvm`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `loongarch64-unknown-linux-musl`
- [zig](https://ziglang.org/) 0.16 and [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
- For packaging: `makensis` (Windows installers), Python 3.11+, `bsdtar` (Debian / Ubuntu: `libarchive-tools`); nfpm is downloaded and verified by the script
- `llvm-readobj` to check the Windows binaries (Debian / Ubuntu: `llvm`)

```bash
./scripts/setup-toolchain.sh          # check and install Rust targets and tools
./scripts/build.sh                    # build all 5 targets and package them into dist/
TARGETS="win-x64 linux-x64" ./scripts/build.sh   # build only some targets
NO_PACKAGES=1 ./scripts/build.sh      # binaries only, no packages
```

Checks during development:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --bins
```

---

## Contributing

Issues and PRs are welcome, especially for HomePod stereo pairs, multi-room and macOS support.

---

## Where the name comes from

The name comes from two rhythm game songs:

- [Nýx](https://music-am.sega.jp/songs/song-01340/): 7mai (ONGEKI)
- [Ref:rain (for 7th Heaven)](https://music-am.sega.jp/songs/song-02462/): カモメサノエレクトリックオーケストラ include Limonène (maimai DX)

---

## License

[MIT](LICENSE) © 2026 pStrikeZ and contributors

The icons and the placeholder cover are from [Microsoft Fluent Emoji](https://github.com/microsoft/fluentui-emoji) (MIT); see [NOTICE](NOTICE) and [LICENSE-fluentui-emoji](LICENSE-fluentui-emoji).
