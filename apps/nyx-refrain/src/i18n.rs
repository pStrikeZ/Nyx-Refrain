//! Internationalization (i18n) for Nyx Refrain GUI (zh-CN and English).

use crate::engine::{EngineError, FirewallError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    ZhCn,
    En,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    AppName,
    StateIdle,
    StateConnecting,
    StateStreaming,
    TrayTooltipIdle,
    TrayTooltipConnecting,
    TrayTooltipStreaming,
    TrayTooltipError,
    TrayStart,
    TrayStop,
    TrayShow,
    TrayLanguage,
    TrayQuit,
    TargetDevice,
    NoDeviceSelected,
    Refresh,
    Searching,
    NoDevicesDiscovered,
    ManualAddressDisclosure,
    DeviceAddress,
    AddressHint,
    LatencyProfile,
    ProfileLow,
    ProfileBalanced,
    ProfileStable,
    ProfileCustom,
    CustomLatencyLabel,
    ClockDrift,
    StatBuffer,
    StatSent,
    StatRetransmits,
    StatGaps,
    StatDropped,
    Volume,
    StartStreaming,
    StopStreaming,
    StatsCaption,
    FirewallBlocked,
    FirewallUnconfigured,
    FirewallWaitingUac,
    FirewallSetupFailed,
    FirewallAllowButton,
    FirewallRetryButton,
    LangAuto,
    LangZhCn,
    LangEn,
    Autostart,
    AutostartError,
    SendNowPlaying,
    RemoteControl,
    ResumeOnLaunch,
    CaptureModeEndpoint,
    CaptureModeEndpointHint,
}

impl Key {
    #[allow(dead_code)]
    pub const ALL: [Key; 53] = [
        Key::AppName,
        Key::StateIdle,
        Key::StateConnecting,
        Key::StateStreaming,
        Key::TrayTooltipIdle,
        Key::TrayTooltipConnecting,
        Key::TrayTooltipStreaming,
        Key::TrayTooltipError,
        Key::TrayStart,
        Key::TrayStop,
        Key::TrayShow,
        Key::TrayLanguage,
        Key::TrayQuit,
        Key::TargetDevice,
        Key::NoDeviceSelected,
        Key::Refresh,
        Key::Searching,
        Key::NoDevicesDiscovered,
        Key::ManualAddressDisclosure,
        Key::DeviceAddress,
        Key::AddressHint,
        Key::LatencyProfile,
        Key::ProfileLow,
        Key::ProfileBalanced,
        Key::ProfileStable,
        Key::ProfileCustom,
        Key::CustomLatencyLabel,
        Key::ClockDrift,
        Key::StatBuffer,
        Key::StatSent,
        Key::StatRetransmits,
        Key::StatGaps,
        Key::StatDropped,
        Key::Volume,
        Key::StartStreaming,
        Key::StopStreaming,
        Key::StatsCaption,
        Key::FirewallBlocked,
        Key::FirewallUnconfigured,
        Key::FirewallWaitingUac,
        Key::FirewallSetupFailed,
        Key::FirewallAllowButton,
        Key::FirewallRetryButton,
        Key::LangAuto,
        Key::LangZhCn,
        Key::LangEn,
        Key::Autostart,
        Key::AutostartError,
        Key::SendNowPlaying,
        Key::RemoteControl,
        Key::ResumeOnLaunch,
        Key::CaptureModeEndpoint,
        Key::CaptureModeEndpointHint,
    ];
}

pub fn t(lang: Lang, key: Key) -> &'static str {
    match (lang, key) {
        // App name
        (_, Key::AppName) => "Nyx Refrain",

        // States
        (Lang::ZhCn, Key::StateIdle) => "空闲",
        (Lang::En, Key::StateIdle) => "Idle",
        (Lang::ZhCn, Key::StateConnecting) => "连接中…",
        (Lang::En, Key::StateConnecting) => "Connecting…",
        (Lang::ZhCn, Key::StateStreaming) => "推流中",
        (Lang::En, Key::StateStreaming) => "Streaming",

        // Tray tooltip
        (Lang::ZhCn, Key::TrayTooltipIdle) => "Nyx Refrain：空闲",
        (Lang::En, Key::TrayTooltipIdle) => "Nyx Refrain: Idle",
        (Lang::ZhCn, Key::TrayTooltipConnecting) => "Nyx Refrain：连接中",
        (Lang::En, Key::TrayTooltipConnecting) => "Nyx Refrain: Connecting",
        (Lang::ZhCn, Key::TrayTooltipStreaming) => "Nyx Refrain：推流中",
        (Lang::En, Key::TrayTooltipStreaming) => "Nyx Refrain: Streaming",
        (Lang::ZhCn, Key::TrayTooltipError) => "Nyx Refrain：出错",
        (Lang::En, Key::TrayTooltipError) => "Nyx Refrain: Error",

        // Tray menu items
        (Lang::ZhCn, Key::TrayStart) => "开始推流",
        (Lang::En, Key::TrayStart) => "Start Streaming",
        (Lang::ZhCn, Key::TrayStop) => "停止推流",
        (Lang::En, Key::TrayStop) => "Stop Streaming",
        (Lang::ZhCn, Key::TrayShow) => "显示窗口",
        (Lang::En, Key::TrayShow) => "Show Window",
        (Lang::ZhCn, Key::TrayLanguage) => "语言",
        (Lang::En, Key::TrayLanguage) => "Language",
        (Lang::ZhCn, Key::TrayQuit) => "退出",
        (Lang::En, Key::TrayQuit) => "Quit",

        // Devices
        (Lang::ZhCn, Key::TargetDevice) => "目标设备",
        (Lang::En, Key::TargetDevice) => "Target Device",
        (Lang::ZhCn, Key::NoDeviceSelected) => "（未选择）",
        (Lang::En, Key::NoDeviceSelected) => "(Not selected)",
        (Lang::ZhCn, Key::Refresh) => "刷新",
        (Lang::En, Key::Refresh) => "Refresh",
        (Lang::ZhCn, Key::Searching) => "搜索中…",
        (Lang::En, Key::Searching) => "Searching…",
        (Lang::ZhCn, Key::NoDevicesDiscovered) => "未发现设备，可在下方手动填写地址",
        (Lang::En, Key::NoDevicesDiscovered) => "No devices found. Enter address below.",
        (Lang::ZhCn, Key::ManualAddressDisclosure) => "手动输入设备地址",
        (Lang::En, Key::ManualAddressDisclosure) => "Manual device address",
        (Lang::ZhCn, Key::DeviceAddress) => "设备地址",
        (Lang::En, Key::DeviceAddress) => "Device Address",
        (Lang::ZhCn, Key::AddressHint) => "192.0.2.106 或 192.0.2.106:7000",
        (Lang::En, Key::AddressHint) => "192.0.2.106 or 192.0.2.106:7000",

        // Latency profile
        (Lang::ZhCn, Key::LatencyProfile) => "延迟档位",
        (Lang::En, Key::LatencyProfile) => "Latency Profile",
        (Lang::ZhCn, Key::ProfileLow) => "低延迟",
        (Lang::En, Key::ProfileLow) => "Low",
        (Lang::ZhCn, Key::ProfileBalanced) => "均衡",
        (Lang::En, Key::ProfileBalanced) => "Balanced",
        (Lang::ZhCn, Key::ProfileStable) => "稳定",
        (Lang::En, Key::ProfileStable) => "Stable",
        (Lang::ZhCn, Key::ProfileCustom) => "自定义",
        (Lang::En, Key::ProfileCustom) => "Custom",
        (Lang::ZhCn, Key::CustomLatencyLabel) => "自定义延迟",
        (Lang::En, Key::CustomLatencyLabel) => "Custom Latency",
        (Lang::ZhCn, Key::ClockDrift) => "时钟漂移",
        (Lang::En, Key::ClockDrift) => "Drift",
        (Lang::ZhCn, Key::StatBuffer) => "缓冲",
        (Lang::En, Key::StatBuffer) => "Buffer",
        (Lang::ZhCn, Key::StatSent) => "已发包",
        (Lang::En, Key::StatSent) => "Sent",
        (Lang::ZhCn, Key::StatRetransmits) => "重传请求",
        (Lang::En, Key::StatRetransmits) => "Resend requests",
        (Lang::ZhCn, Key::StatGaps) => "断流",
        (Lang::En, Key::StatGaps) => "Gaps",
        (Lang::ZhCn, Key::StatDropped) => "丢弃",
        (Lang::En, Key::StatDropped) => "Dropped",

        // Volume
        (Lang::ZhCn, Key::Volume) => "音量",
        (Lang::En, Key::Volume) => "Volume",

        // Streaming action
        (Lang::ZhCn, Key::StartStreaming) => "开始推流",
        (Lang::En, Key::StartStreaming) => "Start Streaming",
        (Lang::ZhCn, Key::StopStreaming) => "停止推流",
        (Lang::En, Key::StopStreaming) => "Stop Streaming",

        // Stats caption
        (Lang::ZhCn, Key::StatsCaption) => "传输状态",
        (Lang::En, Key::StatsCaption) => "Streaming Stats",

        // Firewall banner
        (Lang::ZhCn, Key::FirewallBlocked) => {
            "Windows 防火墙里有一条阻止本程序的规则（可能是首次运行时点了取消），HomePod 的回连会被拦截。"
        }
        (Lang::En, Key::FirewallBlocked) => {
            "Windows Firewall has a rule blocking this app (perhaps cancelled on first run). HomePod return connection will be blocked."
        }
        (Lang::ZhCn, Key::FirewallUnconfigured) => {
            "Windows 防火墙还没有放行本程序，HomePod 的回连可能被拦截（表现为连接约 30 秒后失败）。"
        }
        (Lang::En, Key::FirewallUnconfigured) => {
            "Windows Firewall has not allowed this app yet. HomePod return connection may be blocked (manifests as failure after ~30s)."
        }
        (Lang::ZhCn, Key::FirewallWaitingUac) => "等待管理员授权…（请在弹出的 UAC 窗口中确认）",
        (Lang::En, Key::FirewallWaitingUac) => {
            "Waiting for administrator approval… (confirm in the UAC prompt)"
        }
        (Lang::ZhCn, Key::FirewallSetupFailed) => {
            "连接在 SETUP 阶段失败（500），这通常是防火墙拦截了 HomePod 的回连。"
        }
        (Lang::En, Key::FirewallSetupFailed) => {
            "Connection failed at SETUP (500), typically caused by firewall blocking HomePod return traffic."
        }
        (Lang::ZhCn, Key::FirewallAllowButton) => "🛡 一键放行（需要管理员权限）",
        (Lang::En, Key::FirewallAllowButton) => "🛡 Allow in Firewall (Admin required)",
        (Lang::ZhCn, Key::FirewallRetryButton) => "重试",
        (Lang::En, Key::FirewallRetryButton) => "Retry",

        // Language selection
        (Lang::ZhCn, Key::LangAuto) => "自动（跟随系统）",
        (Lang::En, Key::LangAuto) => "Auto (System)",
        (_, Key::LangZhCn) => "简体中文",
        (_, Key::LangEn) => "English",

        // Autostart
        (Lang::ZhCn, Key::Autostart) => "开机自启",
        (Lang::En, Key::Autostart) => "Launch at login",
        (Lang::ZhCn, Key::AutostartError) => "开机自启设置失败",
        (Lang::En, Key::AutostartError) => "Failed to update launch at login setting",

        // Send now playing
        (Lang::ZhCn, Key::SendNowPlaying) => "推送曲目信息",
        (Lang::En, Key::SendNowPlaying) => "Send now playing",
        (Lang::ZhCn, Key::RemoteControl) => "允许接收端控制播放器",
        (Lang::En, Key::RemoteControl) => "Allow receiver playback controls",
        (Lang::ZhCn, Key::ResumeOnLaunch) => "启动时恢复上次推流",
        (Lang::En, Key::ResumeOnLaunch) => "Resume streaming on launch",

        (Lang::ZhCn, Key::CaptureModeEndpoint) => "使用设备回环采集",
        (Lang::En, Key::CaptureModeEndpoint) => "Use device loopback capture",
        (Lang::ZhCn, Key::CaptureModeEndpointHint) => {
            "默认按进程采集；设备回环会带上扬声器音效和外放音量；推流中切换会自动重连"
        }
        (Lang::En, Key::CaptureModeEndpointHint) => {
            "Default is per-process capture; device loopback includes speaker effects and volume. Switching while streaming reconnects"
        }
    }
}

const NOT_LOCAL_ZH: &str = "接收端不在本机所在的局域网网段内。经 VPN / TUN 访问时 AirPlay 无法工作，因为接收端需要主动连回本机";
const NOT_LOCAL_EN: &str = "the receiver is not on this PC's local network. AirPlay does not work through a VPN / TUN route because the receiver has to connect back to this PC";

/// Short label for the flyout header; the full message goes on its own wrapped line.
pub fn engine_error_title(lang: Lang, err: &EngineError) -> &'static str {
    match (lang, err) {
        (Lang::ZhCn, EngineError::Capture(_)) => "音频采集失败",
        (Lang::En, EngineError::Capture(_)) => "Capture failed",
        (Lang::ZhCn, EngineError::NotSupported(_)) => "设备不受支持",
        (Lang::En, EngineError::NotSupported(_)) => "Unsupported device",
        (Lang::ZhCn, EngineError::Connect(_)) => "连接失败",
        (Lang::En, EngineError::Connect(_)) => "Connection failed",
        (Lang::ZhCn, EngineError::NotLocal(_)) => "无法直连接收端",
        (Lang::En, EngineError::NotLocal(_)) => "Receiver not on this network",
        (Lang::ZhCn, EngineError::Interrupted(_)) => "连接中断",
        (Lang::En, EngineError::Interrupted(_)) => "Connection lost",
    }
}

/// The message under [`engine_error_title`] in the flyout, without repeating the title.
#[cfg_attr(not(windows), allow(dead_code))] // flyout only
pub fn engine_error_detail(lang: Lang, err: &EngineError) -> String {
    match (lang, err) {
        (_, EngineError::Capture(e) | EngineError::Connect(e) | EngineError::Interrupted(e)) => {
            e.clone()
        }
        (Lang::ZhCn, EngineError::NotSupported(m)) => format!("{m}。Nyx Refrain 仅支持 AirPlay 2"),
        (Lang::En, EngineError::NotSupported(m)) => {
            format!("{m}. Nyx Refrain supports AirPlay 2 only")
        }
        (Lang::ZhCn, EngineError::NotLocal(e)) => format!("{}（{e}）", NOT_LOCAL_ZH),
        (Lang::En, EngineError::NotLocal(e)) => format!("{} ({e})", NOT_LOCAL_EN),
    }
}

#[cfg_attr(windows, allow(dead_code))] // Linux tray tooltip (wraps, unlike Windows)
pub fn format_engine_error(lang: Lang, err: &EngineError) -> String {
    match (lang, err) {
        (Lang::ZhCn, EngineError::Capture(e)) => format!("音频采集初始化失败：{e}"),
        (Lang::En, EngineError::Capture(e)) => format!("Audio capture initialization failed: {e}"),
        (Lang::ZhCn, EngineError::NotSupported(m)) => {
            format!("该设备不支持 AirPlay 2（{m}）。Nyx Refrain 仅支持 AirPlay 2")
        }
        (Lang::En, EngineError::NotSupported(m)) => {
            format!("Device does not support AirPlay 2 ({m}). Nyx Refrain supports AirPlay 2 only")
        }
        (Lang::ZhCn, EngineError::Connect(e)) => format!("连接失败：{e}"),
        (Lang::En, EngineError::Connect(e)) => format!("Connection failed: {e}"),
        (Lang::ZhCn, EngineError::NotLocal(e)) => format!("无法直连接收端：{NOT_LOCAL_ZH}（{e}）"),
        (Lang::En, EngineError::NotLocal(e)) => {
            format!("Receiver not on this network: {NOT_LOCAL_EN} ({e})")
        }
        (Lang::ZhCn, EngineError::Interrupted(e)) => format!("连接中断：{e}"),
        (Lang::En, EngineError::Interrupted(e)) => format!("Connection interrupted: {e}"),
    }
}

#[allow(dead_code)]
pub fn format_firewall_error(lang: Lang, err: &FirewallError) -> String {
    match (lang, err) {
        (Lang::ZhCn, FirewallError::Cancelled) => "已取消管理员授权，未做任何修改".into(),
        (Lang::En, FirewallError::Cancelled) => {
            "Administrator permission cancelled, no changes made".into()
        }
        (Lang::ZhCn, FirewallError::InstallFailed(e)) => format!("放行失败：{e}"),
        (Lang::En, FirewallError::InstallFailed(e)) => {
            format!("Failed to add firewall rule: {e}")
        }
        (Lang::ZhCn, FirewallError::CheckFailed(e)) => format!("无法检查防火墙：{e}"),
        (Lang::En, FirewallError::CheckFailed(e)) => format!("Failed to check firewall: {e}"),
        (_, FirewallError::Other(e)) => e.clone(),
    }
}

/// Streaming status row: elapsed time (no label, the ticking clock speaks for itself) and
/// sender/receiver clock drift. The estimated latency is shown on the profile row only.
pub fn format_stats_columns(lang: Lang, secs: u64, drift_ppm: f64) -> [String; 2] {
    [
        format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
        format!("{} {drift_ppm:+.0} ppm", t(lang, Key::ClockDrift)),
    ]
}

/// Label/value pairs shown when the status row is expanded.
#[allow(dead_code)]
pub fn format_stats_detail(
    lang: Lang,
    buffer_fill: usize,
    sent: u64,
    retransmits: u64,
    discontinuities: u64,
    dropped: u64,
) -> [(&'static str, String); 5] {
    let pkts = match lang {
        Lang::ZhCn => "包",
        Lang::En => "pkts",
    };
    [
        (t(lang, Key::StatBuffer), format!("{buffer_fill} {pkts}")),
        (t(lang, Key::StatSent), sent.to_string()),
        (t(lang, Key::StatRetransmits), retransmits.to_string()),
        (t(lang, Key::StatGaps), discontinuities.to_string()),
        (t(lang, Key::StatDropped), dropped.to_string()),
    ]
}

pub fn detect_system_language() -> Lang {
    #[cfg(windows)]
    {
        // Safety: Win32 call with no preconditions.
        let langid = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        lang_from_langid(langid)
    }
    #[cfg(not(windows))]
    {
        let env_val = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        lang_from_env(&env_val)
    }
}

/// Primary language ID extraction: `langid & 0x03ff`.
/// 0x04 = LANG_CHINESE (zh-CN 0x0804, zh-TW 0x0404, zh-HK 0x0c04, zh-SG 0x1004).
#[allow(dead_code)]
pub fn lang_from_langid(langid: u16) -> Lang {
    let primary = langid & 0x03ff;
    if primary == 0x04 {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

#[allow(dead_code)]
pub fn lang_from_env(env: &str) -> Lang {
    let lower = env.to_ascii_lowercase();
    if lower.starts_with("zh") || lower.contains("zh_") || lower.contains("zh-") {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_keys_have_translations() {
        for &key in &Key::ALL {
            let zh = t(Lang::ZhCn, key);
            let en = t(Lang::En, key);
            assert!(!zh.is_empty(), "Empty zh-CN translation for {key:?}");
            assert!(!en.is_empty(), "Empty En translation for {key:?}");
        }
    }

    #[test]
    fn language_from_langid_detection() {
        // Chinese variants (primary LANGID 0x04)
        assert_eq!(lang_from_langid(0x0804), Lang::ZhCn); // zh-CN
        assert_eq!(lang_from_langid(0x0404), Lang::ZhCn); // zh-TW
        assert_eq!(lang_from_langid(0x0c04), Lang::ZhCn); // zh-HK

        // Non-Chinese
        assert_eq!(lang_from_langid(0x0409), Lang::En); // en-US
        assert_eq!(lang_from_langid(0x0809), Lang::En); // en-GB
        assert_eq!(lang_from_langid(0x0411), Lang::En); // ja-JP
        assert_eq!(lang_from_langid(0x0407), Lang::En); // de-DE
    }

    #[test]
    fn language_from_env_detection() {
        assert_eq!(lang_from_env("zh_CN.UTF-8"), Lang::ZhCn);
        assert_eq!(lang_from_env("zh_TW.UTF-8"), Lang::ZhCn);
        assert_eq!(lang_from_env("zh-Hans-CN"), Lang::ZhCn);
        assert_eq!(lang_from_env("en_US.UTF-8"), Lang::En);
        assert_eq!(lang_from_env("C"), Lang::En);
        assert_eq!(lang_from_env(""), Lang::En);
    }

    #[test]
    fn format_helpers_produce_valid_strings() {
        let err = EngineError::Connect("timed out".into());
        let zh_err = format_engine_error(Lang::ZhCn, &err);
        let en_err = format_engine_error(Lang::En, &err);
        assert!(zh_err.contains("连接失败"));
        assert!(zh_err.contains("timed out"));
        assert!(en_err.contains("Connection failed"));
        assert!(en_err.contains("timed out"));

        let fw_err = FirewallError::Cancelled;
        assert_eq!(
            format_firewall_error(Lang::ZhCn, &fw_err),
            "已取消管理员授权，未做任何修改"
        );
        assert_eq!(
            format_firewall_error(Lang::En, &fw_err),
            "Administrator permission cancelled, no changes made"
        );

        let [time, drift] = format_stats_columns(Lang::ZhCn, 3665, -837.4);
        assert_eq!(time, "01:01:05");
        assert_eq!(drift, "时钟漂移 -837 ppm");
        let en = format_stats_columns(Lang::En, 5, 12.3);
        assert_eq!(en[1], "Drift +12 ppm");

        let detail = format_stats_detail(Lang::ZhCn, 3, 100, 2, 1, 0);
        assert_eq!(detail[0], ("缓冲", "3 包".to_string()));
        assert_eq!(format_stats_detail(Lang::En, 3, 0, 0, 0, 0)[0].1, "3 pkts");
    }
}
