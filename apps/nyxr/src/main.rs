mod ap2run;
mod config;

use clap::{Parser, Subcommand};
use config::NyxRefrainConfig;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

use capture::AudioSource;
use capture::sources::silence::SilenceSource;
use capture::sources::sine::SineSource;

#[derive(Parser, Debug)]
#[command(
    name = "nyxr",
    version = env!("NYX_VERSION"),
    about = "Low-latency system audio streamer for AirPlay 2 realtime receivers"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Stream live system audio or synthetic audio to an AirPlay target
    Stream {
        /// Target device IP, IP:PORT, or mDNS device name (e.g. "Living Room").
        #[arg(long)]
        target: Option<String>,

        /// Restrict connection and packets to a specific network interface
        #[arg(long)]
        interface: Option<String>,

        /// Initial volume: 0 - 100 percent (0 = mute / -144 dB, 1..=100 mapped to -30..0 dB), or negative dB (e.g. -20)
        #[arg(long, allow_hyphen_values = true)]
        volume: Option<f32>,

        /// Audio capture source (pipewire, wasapi, process, sine, wav, stdin). `process` (Windows):
        /// per-application loopback of everything except nyxr, taken before the output device's
        /// effects and volume; with `--device <PID>` only that process tree
        #[arg(long)]
        source: Option<String>,

        /// Sample rate of raw PCM on stdin (`--source stdin`), e.g. `ffmpeg ... -ar 44100`
        #[arg(long, default_value_t = 44_100)]
        input_rate: u32,

        /// Sample format of raw PCM on stdin: s16le or f32le
        #[arg(long, value_enum, default_value_t = StdinFmtArg::S16le)]
        input_format: StdinFmtArg,

        /// Channel count of raw PCM on stdin
        #[arg(long, default_value_t = 2)]
        input_channels: u16,

        /// Audio device name or ID (or WAV file path when --source wav; on Linux overrides virtual sink description)
        #[arg(long)]
        device: Option<String>,

        /// Do not configure the virtual sink as the default audio output in PipeWire
        #[arg(long)]
        no_set_default: bool,

        /// Print periodic pipeline telemetry metrics as JSON lines
        #[arg(long)]
        stats: bool,

        /// Force quiet mode (-144 dBFS initial volume)
        #[arg(long, conflicts_with = "audible")]
        quiet: bool,

        /// Explicitly enable audible stream mode
        #[arg(long, conflicts_with = "quiet")]
        audible: bool,

        /// Stream duration in seconds (runs until Ctrl+C if omitted)
        #[arg(long)]
        duration: Option<u64>,

        /// Send system now-playing metadata (track title, artist, album, artwork) to AirPlay 2 receiver
        #[arg(long)]
        now_playing: bool,

        /// Disable receiver playback controls while still sending now-playing metadata
        #[arg(long)]
        no_remote_control: bool,

        /// Experiment: read AP2 requests to send from stdin while streaming (type `help`)
        #[arg(long, hide = true)]
        debug_stdin: bool,

        /// AirPlay 2: send the placeholder `progress` (1 hour) before RECORD (opt-in escape hatch)
        #[arg(long)]
        initial_progress: bool,

        /// AirPlay 2 latency preset: low ≈97 ms, balanced ≈137 ms, stable ≈237 ms end-to-end
        #[arg(long, value_enum, default_value_t = ap2run::LatencyProfile::Balanced)]
        latency_profile: ap2run::LatencyProfile,

        /// AirPlay 2 sync latency in ms (overrides --latency-profile; may be negative, >= -165)
        #[arg(long, allow_hyphen_values = true)]
        ap2_sync_latency_ms: Option<i32>,

        /// Optional path to nyx-refrain config.toml
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Print Windows Firewall configuration commands
    FirewallRule {
        /// Print the netsh commands (with this executable's real path) without executing them
        #[arg(long)]
        print: bool,

        /// Show whether Windows Firewall lets receivers connect back to this program
        #[arg(long)]
        check: bool,

        /// Allow inbound traffic for the Nyx Refrain executables in this folder (UAC prompt);
        /// also removes block rules left by a dismissed "Allow access" prompt
        #[arg(long)]
        install: bool,
    },

    /// List available network interfaces with classification and reason
    ListInterfaces,

    /// Discover AirPlay / RAOP devices on the local network via mDNS
    Discover {
        /// Discovery timeout duration (e.g. "3s", "500ms", "5")
        #[arg(long, default_value = "3s")]
        timeout: String,

        /// Print full raw TXT records for all discovered devices
        #[arg(long)]
        dump_txt: bool,

        /// Restrict mDNS browsing to a specific network interface
        #[arg(long)]
        interface: Option<String>,
    },

    /// List cached AirPlay devices from devices.toml
    ListDevices,

    /// Print the system's now-playing info (SMTC / MPRIS) as it changes, without streaming
    NowPlaying,

    /// Stream a test tone or silence to an AirPlay target
    TestTone {
        /// Target device IP or IP:PORT (default port: 7000)
        #[arg(long)]
        target: String,

        /// Target name for session logging
        #[arg(long)]
        name: String,

        /// Restrict connection and packets to a specific network interface
        #[arg(long)]
        interface: Option<String>,

        /// Stream duration in seconds (runs until Ctrl+C if omitted)
        #[arg(long)]
        duration: Option<u64>,

        /// Force quiet mode (-144 dBFS initial volume, digital silence). Note: quiet mode is already the default.
        #[arg(long, conflicts_with = "audible")]
        quiet: bool,

        /// Enable audible tone mode (-20 dBFS sine wave).
        #[arg(long, conflicts_with = "quiet")]
        audible: bool,

        /// Append full RTSP requests and responses to transcript file
        #[arg(long)]
        transcript: Option<PathBuf>,

        /// Number of beeps at stream start (default 0 = continuous tone)
        #[arg(long, default_value_t = 0)]
        beeps: usize,

        /// Tone frequency in Hz (default 440.0)
        #[arg(long, default_value_t = 440.0)]
        freq: f32,

        /// Tone digital amplitude in dBFS (default -20.0)
        #[arg(long, default_value_t = -20.0, allow_hyphen_values = true)]
        dbfs: f32,

        /// Initial volume: 0 - 100 percent (0 = mute / -144 dB, 1..=100 mapped to -30..0 dB), or negative dB (e.g. -20)
        #[arg(long, allow_hyphen_values = true)]
        volume: Option<f32>,

        /// Print session metrics upon completion
        #[arg(long)]
        stats: bool,

        /// AirPlay 2: send the placeholder `progress` (1 hour) before RECORD (opt-in escape hatch)
        #[arg(long)]
        initial_progress: bool,

        /// Experiment: read AP2 requests to send from stdin while streaming (type `help`)
        #[arg(long, hide = true)]
        debug_stdin: bool,

        /// AirPlay 2: send this track title as now-playing text (with --track-artist/--track-album)
        #[arg(long)]
        track_title: Option<String>,

        /// AirPlay 2: now-playing artist (used with --track-title)
        #[arg(long, default_value = "")]
        track_artist: String,

        /// AirPlay 2: now-playing album (used with --track-title)
        #[arg(long, default_value = "")]
        track_album: String,

        /// AirPlay 2 latency preset: low ≈97 ms, balanced ≈137 ms, stable ≈237 ms end-to-end
        #[arg(long, value_enum, default_value_t = ap2run::LatencyProfile::Balanced)]
        latency_profile: ap2run::LatencyProfile,

        /// AirPlay 2 sync latency in ms (overrides --latency-profile; may be negative, >= -165)
        #[arg(long, allow_hyphen_values = true)]
        ap2_sync_latency_ms: Option<i32>,
    },
}

fn parse_duration_str(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        Ok(Duration::from_millis(num.parse()?))
    } else if let Some(num) = s.strip_suffix('s') {
        Ok(Duration::from_secs(num.parse()?))
    } else {
        Ok(Duration::from_secs(s.parse()?))
    }
}

async fn resolve_target_device(
    target_str: &str,
    interface: Option<&str>,
) -> anyhow::Result<(SocketAddr, String, Option<String>)> {
    if let Ok((ip, port)) = discovery::parse_target_addr(target_str) {
        let addr = SocketAddr::from((ip, port));
        let (name, features) = if ip.is_loopback() {
            ("loopback".to_string(), None)
        } else {
            let cache = discovery::DeviceCache::load_default();
            if let Some(dev) = cache.find_by_ip(ip) {
                let feat = dev
                    .txt_records
                    .get("features")
                    .or_else(|| dev.txt_records.get("ft"))
                    .cloned();
                (dev.name.clone(), feat)
            } else {
                (ip.to_string(), None)
            }
        };
        return Ok((addr, name, features));
    }

    let cache = discovery::DeviceCache::load_default();
    if let Some(dev) = cache.find_by_name(target_str) {
        let addr = SocketAddr::from((dev.ip, dev.port));
        let feat = dev
            .txt_records
            .get("features")
            .or_else(|| dev.txt_records.get("ft"))
            .cloned();
        return Ok((addr, dev.name.clone(), feat));
    }

    info!(
        "Target '{}' not found in cache; discovering on local network...",
        target_str
    );
    let opts = discovery::DiscoveryOptions {
        timeout: Duration::from_secs(3),
        interface: interface.map(|s| s.to_string()),
        cache_fallback: true,
        dump_txt: false,
        cache_path: None,
    };
    let devices = discovery::discover_devices(&opts).await?;
    for dev in &devices {
        if dev.name == target_str || dev.instance_name.starts_with(target_str) {
            let addr = SocketAddr::from((dev.ip, dev.port));
            let feat = dev
                .txt_records
                .get("features")
                .or_else(|| dev.txt_records.get("ft"))
                .cloned();
            return Ok((addr, dev.name.clone(), feat));
        }
    }

    anyhow::bail!(
        "Could not resolve target '{}'. Ensure device is powered on, or specify IP directly (e.g. --target 192.0.2.106).",
        target_str
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum StdinFmtArg {
    S16le,
    F32le,
}

/// Raw PCM layout for `--source stdin`.
#[derive(Clone, Copy, Debug)]
struct StdinOpts {
    rate: u32,
    format: StdinFmtArg,
    channels: u16,
}

#[cfg(target_os = "linux")]
type VolCtrl = Option<capture::pipewire::SinkVolumeControl>;
#[cfg(not(target_os = "linux"))]
type VolCtrl = ();

fn create_audio_source(
    source_name: &str,
    device: Option<&str>,
    stdin: StdinOpts,
    no_set_default: bool,
    #[cfg(target_os = "linux")] initial_volume: Option<capture::pipewire::SinkVolume>,
) -> anyhow::Result<(Box<dyn capture::AudioSource>, VolCtrl)> {
    match source_name.to_lowercase().as_str() {
        "sine" => Ok((
            Box::new(capture::sources::sine::SineSource::new(
                440.0, -20.0, 48000, 2,
            )),
            #[cfg(target_os = "linux")]
            None,
            #[cfg(not(target_os = "linux"))]
            (),
        )),
        "silence" => Ok((
            Box::new(capture::sources::silence::SilenceSource::new(48000, 2)),
            #[cfg(target_os = "linux")]
            None,
            #[cfg(not(target_os = "linux"))]
            (),
        )),
        "stdin" => Ok((
            Box::new(capture::sources::stdin::StdinSource::from_stdin(
                match stdin.format {
                    StdinFmtArg::S16le => capture::sources::stdin::StdinFormat::S16Le,
                    StdinFmtArg::F32le => capture::sources::stdin::StdinFormat::F32Le,
                },
                stdin.rate,
                stdin.channels,
            )),
            #[cfg(target_os = "linux")]
            None,
            #[cfg(not(target_os = "linux"))]
            (),
        )),
        "wav" => {
            let wav_path = device.ok_or_else(|| {
                anyhow::anyhow!("--device <path_to_wav> is required when --source is wav")
            })?;
            let wav = capture::sources::wav::FileWavSource::open(wav_path)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok((
                Box::new(wav),
                #[cfg(target_os = "linux")]
                None,
                #[cfg(not(target_os = "linux"))]
                (),
            ))
        }
        "pipewire" => {
            #[cfg(target_os = "linux")]
            {
                let (node_name, node_description) = match device {
                    Some(desc) => {
                        let sanitized = desc
                            .to_lowercase()
                            .chars()
                            .map(|c| {
                                if c.is_alphanumeric() || c == '_' || c == '-' {
                                    c
                                } else {
                                    '_'
                                }
                            })
                            .collect::<String>();
                        let trimmed = sanitized.trim_matches('_');
                        let name = if trimmed.is_empty() {
                            "nyx_refrain_airplay".to_string()
                        } else {
                            format!("nyx_refrain_{trimmed}")
                        };
                        (name, desc.to_string())
                    }
                    None => (
                        "nyx_refrain_airplay".to_string(),
                        "Nyx Refrain (AirPlay)".to_string(),
                    ),
                };
                let config = capture::pipewire::PipeWireSinkConfig {
                    node_name,
                    node_description,
                    set_default: !no_set_default,
                    initial_volume,
                };
                let source = capture::pipewire::PipeWireSinkSource::new(config)
                    .map_err(|e| anyhow::anyhow!("PipeWire virtual sink init failed: {e}"))?;
                let ctrl = source.volume_control();
                Ok((Box::new(source), Some(ctrl)))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = no_set_default;
                anyhow::bail!(
                    "PipeWire capture is only available on Linux; use --source wasapi on Windows, or sine/wav/stdin"
                )
            }
        }
        "wasapi" => {
            #[cfg(windows)]
            {
                let _ = no_set_default;
                use capture::wasapi::endpoint::DeviceSpec;
                // Endpoint IDs from IMMDevice::GetId look like "{0.0.0.00000000}.{GUID}";
                // anything else is treated as a friendly-name substring.
                let device_spec = match device {
                    None => DeviceSpec::Default,
                    Some(d) if d.starts_with('{') => DeviceSpec::ById(d.to_string()),
                    Some(d) => DeviceSpec::ByName(d.to_string()),
                };
                let config = capture::wasapi::WasapiLoopbackConfig {
                    device_spec,
                    ..Default::default()
                };
                let source = capture::wasapi::WasapiLoopbackSource::with_config(config)
                    .map_err(|e| anyhow::anyhow!("WASAPI loopback init failed: {e}"))?;
                Ok((
                    Box::new(source),
                    #[cfg(target_os = "linux")]
                    None,
                    #[cfg(not(target_os = "linux"))]
                    (),
                ))
            }
            #[cfg(not(windows))]
            {
                let _ = no_set_default;
                anyhow::bail!(
                    "WASAPI loopback capture is only available on Windows; use --source pipewire on Linux, or sine/wav/stdin"
                )
            }
        }
        "process" => {
            #[cfg(windows)]
            {
                let _ = no_set_default;
                use capture::wasapi::{
                    ProcessLoopbackConfig, ProcessLoopbackMode, ProcessLoopbackSource,
                };
                let config = match device {
                    Some(pid) => {
                        let pid: u32 = pid.parse().map_err(|_| {
                            anyhow::anyhow!("--device must be a process ID with --source process")
                        })?;
                        ProcessLoopbackConfig::new(pid)
                    }
                    None => ProcessLoopbackConfig {
                        mode: ProcessLoopbackMode::ExcludeProcessTree,
                        ..ProcessLoopbackConfig::new(std::process::id())
                    },
                };
                let source = ProcessLoopbackSource::with_config(config)
                    .map_err(|e| anyhow::anyhow!("process loopback init failed: {e}"))?;
                Ok((Box::new(source), ()))
            }
            #[cfg(not(windows))]
            {
                let _ = no_set_default;
                anyhow::bail!("process loopback capture is only available on Windows")
            }
        }
        other => anyhow::bail!(
            "Unsupported audio source '{}'; supported sources: pipewire, wasapi, sine, wav, stdin",
            other
        ),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG wins; otherwise `log_level` from the default config.toml (default "info").
    // A bare `fmt::init()` falls back to ERROR only, which hid every progress message.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::try_new(NyxRefrainConfig::load_default().log_level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    #[cfg(windows)]
    {
        // Probe and ensure COM / WASAPI linkage is active on Windows builds
        if let Err(err) = capture::wasapi::init_wasapi() {
            tracing::warn!("WASAPI / COM probe initialization warning: {err}");
        }
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::FirewallRule {
            print,
            check,
            install,
        }) => {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "nyxr.exe".into());
            if print {
                println!("# Nyx Refrain Windows Firewall commands (run in an elevated prompt):");
                println!(
                    "netsh advfirewall firewall add rule name=\"{}\" dir=in action=allow program=\"{exe}\" enable=yes profile=any",
                    netutil::firewall::rule_name(&exe)
                );
            }
            #[cfg(windows)]
            {
                if check || install {
                    let status = netutil::firewall::check(std::path::Path::new(&exe))
                        .map_err(|e| anyhow::anyhow!("firewall check failed: {e}"))?;
                    println!(
                        "firewall enabled: {}, inbound allow rule: {}, inbound block rule: {} -> {}",
                        status.firewall_enabled,
                        status.allowed,
                        status.blocked,
                        if status.needs_fix() {
                            "NEEDS FIX"
                        } else {
                            "OK"
                        }
                    );
                    if install {
                        let programs = netutil::firewall::sibling_programs();
                        println!("Requesting administrator rights to allow: {programs:?}");
                        match netutil::firewall::install_elevated(&programs) {
                            Ok(true) => {
                                let after = netutil::firewall::check(std::path::Path::new(&exe))
                                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                                println!(
                                    "Done. allow: {}, block: {} -> {}",
                                    after.allowed,
                                    after.blocked,
                                    if after.needs_fix() {
                                        "still NEEDS FIX"
                                    } else {
                                        "OK"
                                    }
                                );
                            }
                            Ok(false) => println!("Cancelled at the UAC prompt; nothing changed."),
                            Err(e) => anyhow::bail!("firewall install failed: {e}"),
                        }
                    }
                }
            }
            #[cfg(not(windows))]
            if check || install {
                println!("Windows Firewall handling is only available on Windows.");
            }
            if !print && !check && !install {
                println!("Use --check, --install (UAC prompt) or --print.");
            }
        }
        Some(Commands::Stream {
            target,
            interface,
            volume,
            source,
            input_rate,
            input_format,
            input_channels,
            device,
            no_set_default,
            stats,
            quiet,
            audible,
            duration,
            now_playing,
            no_remote_control,
            debug_stdin,
            initial_progress,
            latency_profile,
            ap2_sync_latency_ms,
            config: config_path,
        }) => {
            let cfg = match config_path {
                Some(ref p) => NyxRefrainConfig::load_from_path(p)?,
                None => NyxRefrainConfig::load_default(),
            };

            let target = target.or(cfg.target).ok_or_else(|| {
                anyhow::anyhow!(
                    "--target is required (specify --target <ip|name> or set in config.toml)"
                )
            })?;

            let target_iface = interface.or(cfg.interface);
            let target_vol = volume.or(cfg.volume);
            let source_name = source.unwrap_or(cfg.capture);

            let effective_quiet = quiet || (!audible && source_name == "silence");
            let volume_db = raop::volume::cli_volume_to_db(target_vol, effective_quiet);

            let stdin_opts = StdinOpts {
                rate: input_rate,
                format: input_format,
                channels: input_channels,
            };
            if source_name == "stdin" {
                info!(
                    "stdin PCM: {:?}, {} Hz, {} ch (set --input-rate/--input-format/--input-channels to match the producer)",
                    input_format, input_rate, input_channels
                );
            }
            #[cfg(target_os = "linux")]
            let initial_sink_volume = if source_name == "pipewire" {
                Some(capture::pipewire::SinkVolume {
                    pct: raop::volume::db_to_pct(volume_db),
                    mute: effective_quiet || volume_db <= -144.0,
                })
            } else {
                None
            };
            #[allow(unused_variables)]
            let (audio_source, vol_ctrl) = create_audio_source(
                &source_name,
                device.as_deref(),
                stdin_opts,
                no_set_default,
                #[cfg(target_os = "linux")]
                initial_sink_volume,
            )?;

            let (target_addr, target_name, target_features) =
                resolve_target_device(&target, target_iface.as_deref()).await?;
            info!(
                "Starting Nyx Refrain stream to {} ({}), source: {}",
                target_name, target_addr, source_name
            );
            let run = ap2run::Ap2Run {
                target: target_addr,
                interface: target_iface,
                volume_db,
                quiet: effective_quiet,
                sync_latency_ms: ap2run::sync_latency_ms(latency_profile, ap2_sync_latency_ms)?,
                duration,
                stats,
                transcript: None,
                now_playing,
                remote_control: !no_remote_control,
                debug_stdin,
                features: target_features,
                initial_progress,
                static_track: None,
                #[cfg(target_os = "linux")]
                volume_control: vol_ctrl,
            };
            ap2run::run_ap2(run, audio_source).await?;
        }
        Some(Commands::ListInterfaces) => {
            let classified = netutil::enumerate_and_classify_interfaces(
                &netutil::ClassificationConfig::default(),
            )?;
            println!("Local Network Interfaces ({})", classified.len());
            println!("{:=<70}", "");
            for c in &classified {
                let status = if c.is_usable_physical {
                    "[USABLE PHYSICAL]"
                } else {
                    "[EXCLUDED]"
                };
                let link_state = if c.iface.is_up { "UP" } else { "DOWN" };
                println!(
                    "Interface #{}: {} ({}) - {}",
                    c.iface.index,
                    c.iface.name,
                    c.iface.display_name(),
                    link_state
                );
                println!("  Type:           {}", c.iface.if_type);
                let addrs: Vec<String> = c.iface.ipv4_nets.iter().map(|n| n.to_string()).collect();
                println!(
                    "  IPv4:           {}",
                    if addrs.is_empty() {
                        "none".to_string()
                    } else {
                        addrs.join(", ")
                    }
                );
                println!("  Classification: {} {}", status, c.reason);
                println!();
            }
        }
        Some(Commands::Discover {
            timeout,
            dump_txt,
            interface,
        }) => {
            let timeout_dur = parse_duration_str(&timeout)?;
            println!(
                "Browsing for AirPlay / RAOP devices via mDNS (timeout {:?}, iface: {})...",
                timeout_dur,
                interface.as_deref().unwrap_or("auto")
            );

            let opts = discovery::DiscoveryOptions {
                timeout: timeout_dur,
                interface,
                cache_fallback: true,
                dump_txt,
                cache_path: None,
            };

            let devices = discovery::discover_devices(&opts).await?;
            if devices.is_empty() {
                println!("No AirPlay devices discovered.");
            } else {
                println!("Discovered {} AirPlay / RAOP device(s):\n", devices.len());
                for (idx, dev) in devices.iter().enumerate() {
                    println!("{}. {}", idx + 1, dev.summary());
                    println!("   Instance:  {}", dev.instance_name);
                    println!("   Address:   {}:{}", dev.ip, dev.port);
                    if let Some(ref mac) = dev.mac {
                        println!("   MAC:       {}", mac);
                    }
                    if let Some(ref host) = dev.host {
                        println!("   Hostname:  {}", host);
                    }
                    if dump_txt {
                        println!("   TXT Records (raw dump):");
                        let mut sorted_keys: Vec<_> = dev.txt_records.keys().collect();
                        sorted_keys.sort();
                        for k in sorted_keys {
                            println!("     {} = {}", k, dev.txt_records[k]);
                        }
                    }
                    println!();
                }
            }
        }
        Some(Commands::NowPlaying) => {
            let watcher = nowplaying::NowPlayingWatcher::start();
            let mut rx = watcher.subscribe();
            println!("Watching now-playing info; press Ctrl+C to stop.");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let np = rx.borrow_and_update().clone();
                        println!("{}", nowplaying::describe(&np, chrono_like_now()));
                    }
                }
            }
        }
        Some(Commands::ListDevices) => {
            let cache_path = discovery::default_cache_path();
            let cache = discovery::DeviceCache::load_default();
            if cache.devices.is_empty() {
                println!("No cached devices found in {:?}", cache_path);
            } else {
                println!(
                    "Cached AirPlay devices from {:?} ({}):\n",
                    cache_path,
                    cache.devices.len()
                );
                for (idx, dev) in cache.devices.iter().enumerate() {
                    println!("{}. {}", idx + 1, dev.summary());
                    println!("   Instance:  {}", dev.instance_name);
                    println!("   Address:   {}:{}", dev.ip, dev.port);
                    if let Some(ref mac) = dev.mac {
                        println!("   MAC:       {}", mac);
                    }
                    println!("   Last seen: {} (epoch seconds)", dev.last_seen_epoch_secs);
                    println!();
                }
            }
        }
        Some(Commands::TestTone {
            target,
            name,
            interface,
            duration,
            quiet: _,
            audible,
            transcript,
            beeps,
            freq,
            dbfs,
            volume,
            stats,
            initial_progress,
            debug_stdin,
            track_title,
            track_artist,
            track_album,
            latency_profile,
            ap2_sync_latency_ms,
        }) => {
            // 1. Parse target SocketAddr (default port 7000)
            let target_addr: SocketAddr = if target.contains(':') {
                target.parse()?
            } else {
                format!("{}:7000", target).parse()?
            };

            let effective_quiet = !audible;

            let volume_db = raop::volume::cli_volume_to_db(volume, effective_quiet);

            info!(
                "Starting session to {} ({}) effective_quiet={} volume_db={:.1}",
                target_addr, name, effective_quiet, volume_db
            );

            // In quiet mode: SilenceSource
            let make_source = || -> Box<dyn AudioSource + Send> {
                if effective_quiet {
                    Box::new(SilenceSource::new(44100, 2))
                } else {
                    Box::new(SineSource::new(freq, dbfs, 44100, 2).with_beeps(beeps))
                }
            };
            if !effective_quiet {
                warn!(
                    "Audible test tone requested ({:.1} dBFS sine {:.1} Hz, beeps={})",
                    dbfs, freq, beeps
                );
            }

            let run = ap2run::Ap2Run {
                target: target_addr,
                interface,
                volume_db,
                quiet: effective_quiet,
                sync_latency_ms: ap2run::sync_latency_ms(latency_profile, ap2_sync_latency_ms)?,
                duration,
                stats,
                transcript,
                now_playing: false,
                remote_control: false,
                debug_stdin,
                features: None,
                initial_progress,
                static_track: track_title
                    .as_deref()
                    .map(|t| raop::ap2::metadata::TrackInfo::new(t, &track_artist, &track_album)),
                #[cfg(target_os = "linux")]
                volume_control: None,
            };
            ap2run::run_ap2(run, make_source()).await?;
        }
        None => {
            println!("nyxr {}", env!("CARGO_PKG_VERSION"));
            println!("Use --help for usage information");
        }
    }

    Ok(())
}

/// Local wall-clock time as `HH:MM:SS.mmm` (UTC offset ignored; only used for relative timing).
fn chrono_like_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03} UTC",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_accepts_one_target_and_airplay2_options() {
        let cli = Cli::try_parse_from([
            "nyxr",
            "stream",
            "--target",
            "127.0.0.1:7000",
            "--source",
            "stdin",
            "--latency-profile",
            "low",
            "--ap2-sync-latency-ms",
            "-100",
            "--now-playing",
        ])
        .unwrap();
        let Some(Commands::Stream {
            target,
            latency_profile,
            ap2_sync_latency_ms,
            now_playing,
            no_remote_control,
            ..
        }) = cli.command
        else {
            panic!("expected stream command");
        };
        assert_eq!(target.as_deref(), Some("127.0.0.1:7000"));
        assert_eq!(latency_profile, ap2run::LatencyProfile::Low);
        assert_eq!(ap2_sync_latency_ms, Some(-100));
        assert!(now_playing);
        assert!(!no_remote_control);
        let disabled =
            Cli::try_parse_from(["nyxr", "stream", "--now-playing", "--no-remote-control"])
                .unwrap();
        assert!(matches!(
            disabled.command,
            Some(Commands::Stream {
                no_remote_control: true,
                ..
            })
        ));

        let err = Cli::try_parse_from([
            "nyxr",
            "stream",
            "--target",
            "127.0.0.1:7000",
            "--target",
            "127.0.0.1:7001",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn removed_airplay1_and_livetest_options_are_rejected() {
        for command in ["stream", "test-tone"] {
            for option in [
                "--protocol",
                "--encryption",
                "--auth-setup",
                "--apple-challenge",
                "--legacy-early-sync",
                "--prefill-ms",
                "--lead-ms",
                "--send-progress",
                "--alac-hassize",
                "--verbose-first-packets",
                "--sync-flag",
                "--latency-ms",
                "--live-config",
            ] {
                let err = Cli::try_parse_from(["nyxr", command, option]).unwrap_err();
                assert_eq!(
                    err.kind(),
                    clap::error::ErrorKind::UnknownArgument,
                    "{command} {option}"
                );
            }
        }
    }

    #[test]
    fn test_tone_defaults_to_quiet_and_rejects_conflicting_modes() {
        let args = [
            "nyxr",
            "test-tone",
            "--target",
            "127.0.0.1:7000",
            "--name",
            "loopback",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        let Some(Commands::TestTone { audible, .. }) = cli.command else {
            panic!("expected test-tone command");
        };
        assert!(!audible);

        let err =
            Cli::try_parse_from(args.into_iter().chain(["--quiet", "--audible"])).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
