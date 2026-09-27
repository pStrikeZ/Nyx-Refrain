//! Windows Firewall check and one-click fix for inbound traffic from AirPlay receivers.
//!
//! AirPlay 2 receivers connect back to the sender (NTP timing requests during SETUP, the
//! events TCP channel, retransmit requests, mDNS responses). If Windows Firewall blocks the
//! executable, SETUP stalls ~30 s and fails with 500. Two failure modes:
//! - no inbound allow rule for the program;
//! - an inbound **block** rule, created when the user dismissed the first-run "Allow access"
//!   prompt; block rules win over allow rules, so they must be removed.
//!
//! Rules are queried/changed with the NetSecurity PowerShell module and JSON output, which is
//! locale independent (unlike `netsh` text output on e.g. Chinese Windows).

use serde::Deserialize;

/// Result of inspecting the firewall for one program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirewallStatus {
    /// At least one firewall profile (Domain/Private/Public) is enabled.
    pub firewall_enabled: bool,
    /// An enabled inbound allow rule exists for the program.
    pub allowed: bool,
    /// An enabled inbound block rule exists for the program (overrides allow rules).
    pub blocked: bool,
}

impl FirewallStatus {
    /// True when inbound traffic to the program is likely to be dropped.
    pub fn needs_fix(&self) -> bool {
        self.firewall_enabled && (self.blocked || !self.allowed)
    }
}

// NetSecurity enum values: Direction Inbound=1; Action Allow=2, Block=4; Enabled True=1.
const DIRECTION_INBOUND: i64 = 1;
const ACTION_ALLOW: i64 = 2;
const ACTION_BLOCK: i64 = 4;

#[derive(Deserialize)]
struct RawProfile {
    #[serde(rename = "Enabled")]
    enabled: i64,
}

#[derive(Deserialize)]
struct RawRule {
    #[serde(rename = "Direction")]
    direction: i64,
    #[serde(rename = "Action")]
    action: i64,
    #[serde(rename = "Enabled")]
    enabled: i64,
}

#[derive(Deserialize)]
struct RawReport {
    #[serde(default)]
    profiles: OneOrMany<RawProfile>,
    #[serde(default)]
    rules: OneOrMany<RawRule>,
}

/// `ConvertTo-Json` emits a bare object for single-element arrays and `null` for empty ones.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
    None(()),
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        OneOrMany::None(())
    }
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            OneOrMany::Many(v) => v,
            OneOrMany::One(t) => vec![t],
            OneOrMany::None(_) => Vec::new(),
        }
    }
}

/// Parses the JSON printed by [`check_script`].
pub fn parse_report(json: &str) -> Result<FirewallStatus, String> {
    let json = json.trim();
    if json.is_empty() {
        return Err("empty PowerShell output".into());
    }
    let raw: RawReport = serde_json::from_str(json).map_err(|e| format!("bad JSON: {e}"))?;
    let profiles = raw.profiles.into_vec();
    let rules = raw.rules.into_vec();
    let inbound = |a: i64| {
        rules
            .iter()
            .any(|r| r.direction == DIRECTION_INBOUND && r.action == a && r.enabled == 1)
    };
    Ok(FirewallStatus {
        firewall_enabled: profiles.iter().any(|p| p.enabled == 1),
        allowed: inbound(ACTION_ALLOW),
        blocked: inbound(ACTION_BLOCK),
    })
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// PowerShell that prints `{"profiles":[...],"rules":[...]}` for `program`.
pub fn check_script(program: &str) -> String {
    format!(
        "$ErrorActionPreference='SilentlyContinue';\
         $p=@(Get-NetFirewallProfile | ForEach-Object {{ @{{Enabled=[int]($_.Enabled -eq 'True')}} }});\
         $r=@(Get-NetFirewallApplicationFilter -Program {prog} | Get-NetFirewallRule | ForEach-Object {{ @{{Direction=[int]$_.Direction;Action=[int]$_.Action;Enabled=[int]($_.Enabled -eq 'True')}} }});\
         @{{profiles=$p;rules=$r}} | ConvertTo-Json -Depth 3 -Compress",
        prog = ps_quote(program)
    )
}

/// Display name of the allow rule created for `program`.
pub fn rule_name(program: &str) -> String {
    let leaf = program.rsplit(['\\', '/']).next().unwrap_or(program);
    format!("Nyx Refrain - {leaf}")
}

/// Elevated PowerShell that removes block rules for each program and (re)creates an inbound
/// allow rule for all profiles. Exits 0 on success, 1 on failure.
pub fn install_script(programs: &[String]) -> String {
    let mut s = String::from("$ErrorActionPreference='Stop';try{");
    for p in programs {
        let q = ps_quote(p);
        let name = ps_quote(&rule_name(p));
        s.push_str(&format!(
            "Get-NetFirewallApplicationFilter -Program {q} -ErrorAction SilentlyContinue | Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object {{ $_.Action -eq 'Block' }} | Remove-NetFirewallRule;\
             Get-NetFirewallRule -DisplayName {name} -ErrorAction SilentlyContinue | Remove-NetFirewallRule;\
             New-NetFirewallRule -DisplayName {name} -Direction Inbound -Program {q} -Action Allow -Profile Any | Out-Null;"
        ));
    }
    s.push_str("exit 0}catch{exit 1}");
    s
}

/// `-EncodedCommand` payload: base64 of the UTF-16LE script.
pub fn encode_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Queries the firewall for `program` (no elevation needed; ~1 s).
    pub fn check(program: &Path) -> Result<FirewallStatus, String> {
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
            .arg(encode_command(&check_script(&program.to_string_lossy())))
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("powershell: {e}"))?;
        parse_report(&String::from_utf8_lossy(&out.stdout))
    }

    /// Nyx Refrain executables next to the running one (CLI and GUI share a folder).
    pub fn sibling_programs() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for name in ["nyxr.exe", "nyx-refrain.exe"] {
                    let p = dir.join(name);
                    if p.exists() {
                        out.push(p);
                    }
                }
            }
            if !out.contains(&exe) {
                out.push(exe);
            }
        }
        out
    }

    /// Shows the UAC prompt and applies [`install_script`]. `Ok(false)` = user declined.
    pub fn install_elevated(programs: &[PathBuf]) -> Result<bool, String> {
        use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        use windows::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::core::{HSTRING, PCWSTR, w};

        let list: Vec<String> = programs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let params = HSTRING::from(format!(
            "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
            encode_command(&install_script(&list))
        ));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: w!("powershell.exe"),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // Safety: `info` and the strings it points to outlive the call.
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(false);
            }
            return Err(format!("ShellExecuteEx: {e}"));
        }
        let mut code = 1u32;
        // Safety: hProcess is valid because SEE_MASK_NOCLOSEPROCESS was set.
        unsafe {
            WaitForSingleObject(info.hProcess, 60_000);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
        }
        if code == 0 {
            Ok(true)
        } else {
            Err(format!("firewall script exited with code {code}"))
        }
    }
}

#[cfg(windows)]
pub use win::{check, install_elevated, sibling_programs};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_allowed_blocked_and_single_objects() {
        let ok = parse_report(
            r#"{"profiles":[{"Enabled":1},{"Enabled":0}],"rules":[{"Direction":1,"Action":2,"Enabled":1}]}"#,
        )
        .unwrap();
        assert_eq!(
            ok,
            FirewallStatus {
                firewall_enabled: true,
                allowed: true,
                blocked: false
            }
        );
        assert!(!ok.needs_fix());

        // Dismissed prompt: a block rule next to an allow rule still needs fixing.
        let blocked = parse_report(
            r#"{"profiles":{"Enabled":1},"rules":[{"Direction":1,"Action":2,"Enabled":1},{"Direction":1,"Action":4,"Enabled":1}]}"#,
        )
        .unwrap();
        assert!(blocked.blocked && blocked.needs_fix());

        let none = parse_report(r#"{"profiles":[{"Enabled":1}],"rules":null}"#).unwrap();
        assert!(!none.allowed && none.needs_fix());

        let off = parse_report(r#"{"profiles":[{"Enabled":0}],"rules":null}"#).unwrap();
        assert!(!off.needs_fix(), "firewall disabled on all profiles");

        // Outbound or disabled rules do not count.
        let other = parse_report(
            r#"{"profiles":[{"Enabled":1}],"rules":[{"Direction":2,"Action":2,"Enabled":1},{"Direction":1,"Action":2,"Enabled":0}]}"#,
        )
        .unwrap();
        assert!(!other.allowed);
        assert!(parse_report("").is_err());
    }

    #[test]
    fn scripts_quote_paths() {
        let p = r"C:\Users\O'Neil\Nyx Refrain\nyx-refrain.exe";
        assert!(check_script(p).contains(r"'C:\Users\O''Neil\Nyx Refrain\nyx-refrain.exe'"));
        let s = install_script(&[p.to_string()]);
        assert!(s.contains("'Nyx Refrain - nyx-refrain.exe'"));
        assert!(s.contains("-Action Allow -Profile Any"));
        assert!(s.ends_with("exit 0}catch{exit 1}"));
        // UTF-16LE base64 of "ab" is "YQBiAA==".
        assert_eq!(encode_command("ab"), "YQBiAA==");
    }
}
