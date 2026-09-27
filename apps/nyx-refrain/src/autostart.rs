//! Platform autostart (launch at login) management for Windows and Linux.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Windows helpers (pure functions)
// ---------------------------------------------------------------------------

/// Builds the Windows registry Run command value: `"<exe_path>" --autostart`.
#[allow(dead_code)]
pub fn build_windows_run_value(exe_path: &Path) -> String {
    format!("\"{}\" --autostart", exe_path.display())
}

/// Extracts the executable path from a Windows Run command line string.
/// Handles quoted strings like `"C:\path\to\app.exe" --autostart` and
/// unquoted strings like `C:\path\to\app.exe --autostart`.
#[allow(dead_code)]
pub fn extract_windows_run_exe(run_value: &str) -> Option<&str> {
    let trimmed = run_value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(end_idx) = rest.find('"') {
            let exe = &rest[..end_idx];
            if !exe.is_empty() {
                return Some(exe);
            }
        } else if !rest.is_empty() {
            return Some(rest);
        }
    } else {
        let exe = match trimmed.find(|c: char| c.is_whitespace()) {
            Some(idx) => &trimmed[..idx],
            None => trimmed,
        };
        if !exe.is_empty() {
            return Some(exe);
        }
    }
    None
}

/// Normalizes a Windows path for case-insensitive comparison (forward slashes to backslashes,
/// removes `\\?\` verbatim prefix, converts to lowercase).
#[allow(dead_code)]
pub fn normalize_windows_path(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.replace('/', "\\").to_lowercase()
}

/// Checks whether a Windows Run command line string points to `current_exe`.
#[allow(dead_code)]
pub fn windows_run_matches_exe(run_value: &str, current_exe: &Path) -> bool {
    let Some(run_exe) = extract_windows_run_exe(run_value) else {
        return false;
    };
    normalize_windows_path(run_exe) == normalize_windows_path(&current_exe.to_string_lossy())
}

// ---------------------------------------------------------------------------
// Linux helpers (pure functions)
// ---------------------------------------------------------------------------

/// Quotes an Exec argument per the Desktop Entry Specification if it contains reserved
/// characters (such as spaces, tabs, quotes, backslashes, dollar signs, backticks).
#[allow(dead_code)]
pub fn quote_desktop_exec(path_str: &str) -> String {
    let needs_quoting = path_str.chars().any(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\\' | '`' | '$' | '>' | '<' | '~' | '|' | '&' | ';' | '?' | '*' | '(' | ')'
            )
    });
    if !needs_quoting {
        return path_str.to_string();
    }
    let mut out = String::with_capacity(path_str.len() + 2);
    out.push('"');
    for c in path_str.chars() {
        match c {
            '"' | '\\' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Builds the contents of the autostart `.desktop` file for Linux.
#[allow(dead_code)]
pub fn build_linux_desktop_entry(exec_cmd: &str) -> String {
    format!(
        "[Desktop Entry]\n\
        Type=Application\n\
        Name=Nyx Refrain\n\
        Exec={exec_cmd} --autostart\n\
        Icon=nyx-refrain\n\
        Terminal=false\n\
        X-GNOME-Autostart-enabled=true\n"
    )
}

/// Parses an existing Desktop Entry's contents to determine if autostart is enabled.
/// "Enabled" = file exists and has no `Hidden=true` and `X-GNOME-Autostart-enabled` is not false.
#[allow(dead_code)]
pub fn is_desktop_entry_enabled(content: &str) -> bool {
    let mut has_hidden_true = false;
    let mut has_autostart_false = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if key.eq_ignore_ascii_case("Hidden") && val.eq_ignore_ascii_case("true") {
                has_hidden_true = true;
            }
            if key.eq_ignore_ascii_case("X-GNOME-Autostart-enabled")
                && val.eq_ignore_ascii_case("false")
            {
                has_autostart_false = true;
            }
        }
    }

    !has_hidden_true && !has_autostart_false
}

/// Resolves the Exec command string for Linux.
/// If `is_in_path` is true (the current executable matches the `nyx-refrain` found in PATH),
/// uses plain `"nyx-refrain"`. Otherwise, uses the absolute path, quoted per spec if needed.
#[allow(dead_code)]
pub fn resolve_linux_exec_cmd(current_exe: &Path, is_in_path: bool) -> String {
    if is_in_path {
        "nyx-refrain".to_string()
    } else {
        let abs_path = if current_exe.is_absolute() {
            current_exe.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(current_exe)
        };
        quote_desktop_exec(&abs_path.to_string_lossy())
    }
}

/// Checks whether `current_exe` matches any `nyx-refrain` binary found in the `PATH` environment variable.
#[allow(dead_code)]
pub fn check_is_in_path(current_exe: &Path, path_env: Option<&std::ffi::OsStr>) -> bool {
    let Some(path_env) = path_env else {
        return false;
    };
    let Ok(current_canon) = current_exe.canonicalize() else {
        return false;
    };
    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join("nyx-refrain");
        if candidate.is_file()
            && let Ok(canon) = candidate.canonicalize()
            && canon == current_canon
        {
            return true;
        }
    }
    false
}

/// Checks if autostart is enabled in the given directory (e.g. `~/.config/autostart`).
#[allow(dead_code)]
pub fn is_linux_autostart_enabled_in(autostart_dir: &Path) -> Result<bool, String> {
    let desktop_path = autostart_dir.join("nyx-refrain.desktop");
    if !desktop_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&desktop_path)
        .map_err(|e| format!("Failed to read autostart desktop entry: {e}"))?;
    Ok(is_desktop_entry_enabled(&content))
}

/// Sets autostart enabled/disabled in the given directory.
#[allow(dead_code)]
pub fn set_linux_autostart_enabled_in(
    autostart_dir: &Path,
    enabled: bool,
    exec_cmd: &str,
) -> Result<(), String> {
    let desktop_path = autostart_dir.join("nyx-refrain.desktop");
    if enabled {
        std::fs::create_dir_all(autostart_dir)
            .map_err(|e| format!("Failed to create autostart directory: {e}"))?;
        let content = build_linux_desktop_entry(exec_cmd);
        std::fs::write(&desktop_path, content)
            .map_err(|e| format!("Failed to write autostart desktop entry: {e}"))?;
    } else if desktop_path.exists() {
        std::fs::remove_file(&desktop_path)
            .map_err(|e| format!("Failed to remove autostart desktop entry: {e}"))?;
    }
    Ok(())
}

/// Returns the user's autostart directory on Linux ($XDG_CONFIG_HOME/autostart or ~/.config/autostart).
#[allow(dead_code)]
pub fn linux_autostart_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(PathBuf::from(xdg).join("autostart"))
    } else {
        std::env::var_os("HOME")
            .filter(|s| !s.is_empty())
            .map(|home| PathBuf::from(home).join(".config").join("autostart"))
    }
}

// ---------------------------------------------------------------------------
// Windows Win32 Registry implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win_reg {
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ,
        REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW,
    };
    use windows::core::{PCWSTR, w};

    const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const APP_VALUE_NAME: PCWSTR = w!("Nyx Refrain");

    struct KeyGuard(HKEY);
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    pub fn is_enabled_windows(current_exe: &Path) -> Result<bool, String> {
        unsafe {
            let mut hkey = HKEY::default();
            let res = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_QUERY_VALUE, &mut hkey);
            if res == ERROR_FILE_NOT_FOUND || res == ERROR_PATH_NOT_FOUND {
                return Ok(false);
            }
            if res != ERROR_SUCCESS {
                return Err(format!(
                    "Failed to open Run registry key: error code {}",
                    res.0
                ));
            }
            let _guard = KeyGuard(hkey);

            let mut val_type = REG_VALUE_TYPE(0);
            let mut byte_len: u32 = 0;
            let query_res = RegQueryValueExW(
                hkey,
                APP_VALUE_NAME,
                None,
                Some(&mut val_type),
                None,
                Some(&mut byte_len),
            );
            if query_res == ERROR_FILE_NOT_FOUND {
                return Ok(false);
            }
            if query_res != ERROR_SUCCESS {
                return Err(format!(
                    "Failed to query Nyx Refrain registry value: error code {}",
                    query_res.0
                ));
            }
            if val_type != REG_SZ && val_type != REG_EXPAND_SZ {
                return Ok(false);
            }
            if byte_len == 0 {
                return Ok(false);
            }

            let mut buf = vec![0u8; byte_len as usize];
            let read_res = RegQueryValueExW(
                hkey,
                APP_VALUE_NAME,
                None,
                Some(&mut val_type),
                Some(buf.as_mut_ptr()),
                Some(&mut byte_len),
            );
            if read_res != ERROR_SUCCESS {
                return Err(format!(
                    "Failed to read Nyx Refrain registry value: error code {}",
                    read_res.0
                ));
            }

            let u16_len = (byte_len as usize) / 2;
            let u16_slice: &[u16] = std::slice::from_raw_parts(buf.as_ptr() as *const u16, u16_len);
            let trimmed_u16 = if let Some(&0) = u16_slice.last() {
                &u16_slice[..u16_slice.len() - 1]
            } else {
                u16_slice
            };
            let run_val = String::from_utf16_lossy(trimmed_u16);
            Ok(super::windows_run_matches_exe(&run_val, current_exe))
        }
    }

    pub fn set_enabled_windows(current_exe: &Path, enabled: bool) -> Result<(), String> {
        unsafe {
            let mut hkey = HKEY::default();
            let res = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_SET_VALUE, &mut hkey);
            if res != ERROR_SUCCESS {
                return Err(format!(
                    "Failed to open Run registry key for writing: error code {}",
                    res.0
                ));
            }
            let _guard = KeyGuard(hkey);

            if enabled {
                let cmd = super::build_windows_run_value(current_exe);
                let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                let set_res = RegSetValueExW(hkey, APP_VALUE_NAME, 0, REG_SZ, Some(bytes));
                if set_res != ERROR_SUCCESS {
                    return Err(format!(
                        "Failed to set Nyx Refrain registry value: error code {}",
                        set_res.0
                    ));
                }
            } else {
                let del_res = RegDeleteValueW(hkey, APP_VALUE_NAME);
                if del_res != ERROR_SUCCESS && del_res != ERROR_FILE_NOT_FOUND {
                    return Err(format!(
                        "Failed to delete Nyx Refrain registry value: error code {}",
                        del_res.0
                    ));
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Platform-neutral public API
// ---------------------------------------------------------------------------

/// Checks whether Nyx Refrain is configured to launch at login.
#[cfg(windows)]
pub fn is_enabled() -> Result<bool, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to determine current executable path: {e}"))?;
    win_reg::is_enabled_windows(&current_exe)
}

/// Enables or disables launch at login for Nyx Refrain.
#[cfg(windows)]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to determine current executable path: {e}"))?;
    win_reg::set_enabled_windows(&current_exe, enabled)
}

/// Checks whether Nyx Refrain is configured to launch at login.
#[cfg(target_os = "linux")]
pub fn is_enabled() -> Result<bool, String> {
    let Some(dir) = linux_autostart_dir() else {
        return Err(
            "Could not determine autostart directory (neither XDG_CONFIG_HOME nor HOME is set)"
                .into(),
        );
    };
    is_linux_autostart_enabled_in(&dir)
}

/// Enables or disables launch at login for Nyx Refrain.
#[cfg(target_os = "linux")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let Some(dir) = linux_autostart_dir() else {
        return Err(
            "Could not determine autostart directory (neither XDG_CONFIG_HOME nor HOME is set)"
                .into(),
        );
    };
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to determine current executable path: {e}"))?;
    let in_path = check_is_in_path(&current_exe, std::env::var_os("PATH").as_deref());
    let exec_cmd = resolve_linux_exec_cmd(&current_exe, in_path);
    set_linux_autostart_enabled_in(&dir, enabled, &exec_cmd)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn is_enabled() -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn set_enabled(_: bool) -> Result<(), String> {
    Err("Autostart is not supported on this platform".into())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_windows_run_value() {
        let p = Path::new(r"C:\Program Files\Nyx Refrain\nyx-refrain.exe");
        assert_eq!(
            build_windows_run_value(p),
            r#""C:\Program Files\Nyx Refrain\nyx-refrain.exe" --autostart"#
        );

        let p2 = Path::new(r"C:\nyx\nyx-refrain.exe");
        assert_eq!(
            build_windows_run_value(p2),
            r#""C:\nyx\nyx-refrain.exe" --autostart"#
        );
    }

    #[test]
    fn test_extract_windows_run_exe() {
        assert_eq!(
            extract_windows_run_exe(
                r#""C:\Program Files\Nyx Refrain\nyx-refrain.exe" --autostart"#
            ),
            Some(r"C:\Program Files\Nyx Refrain\nyx-refrain.exe")
        );
        assert_eq!(
            extract_windows_run_exe(r#"C:\Nyx\nyx-refrain.exe --autostart"#),
            Some(r"C:\Nyx\nyx-refrain.exe")
        );
        assert_eq!(
            extract_windows_run_exe(r#"  "D:\app\test.exe"  "#),
            Some(r"D:\app\test.exe")
        );
        assert_eq!(extract_windows_run_exe(""), None);
        assert_eq!(extract_windows_run_exe(r#""""#), None);
    }

    #[test]
    fn test_windows_run_matches_exe() {
        let cur = Path::new(r"C:\Program Files\Nyx Refrain\nyx-refrain.exe");

        // Exact match
        assert!(windows_run_matches_exe(
            r#""C:\Program Files\Nyx Refrain\nyx-refrain.exe" --autostart"#,
            cur
        ));

        // Case insensitivity
        assert!(windows_run_matches_exe(
            r#""c:\program files\nyx refrain\NYX-REFRAIN.EXE" --autostart"#,
            cur
        ));

        // Slash normalization
        assert!(windows_run_matches_exe(
            r#""C:/Program Files/Nyx Refrain/nyx-refrain.exe" --autostart"#,
            cur
        ));

        // Verbatim path prefix
        assert!(windows_run_matches_exe(
            r#""\\?\C:\Program Files\Nyx Refrain\nyx-refrain.exe" --autostart"#,
            cur
        ));

        // Exe moved / points elsewhere
        assert!(!windows_run_matches_exe(
            r#""C:\Old Location\nyx-refrain.exe" --autostart"#,
            cur
        ));
        assert!(!windows_run_matches_exe("", cur));
    }

    #[test]
    fn test_quote_desktop_exec() {
        // Simple command without spaces or special characters
        assert_eq!(quote_desktop_exec("nyx-refrain"), "nyx-refrain");
        assert_eq!(
            quote_desktop_exec("/usr/bin/nyx-refrain"),
            "/usr/bin/nyx-refrain"
        );

        // Spaces require double quotes
        assert_eq!(
            quote_desktop_exec("/home/user/My Apps/nyx-refrain"),
            r#""/home/user/My Apps/nyx-refrain""#
        );

        // Quotes, backslashes, dollar signs, backticks are escaped
        assert_eq!(
            quote_desktop_exec(r#"/home/user/test"dir\$name/nyx-refrain"#),
            r#""/home/user/test\"dir\\\$name/nyx-refrain""#
        );
    }

    #[test]
    fn test_build_linux_desktop_entry() {
        let entry = build_linux_desktop_entry("nyx-refrain");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("Type=Application\n"));
        assert!(entry.contains("Name=Nyx Refrain\n"));
        assert!(entry.contains("Exec=nyx-refrain --autostart\n"));
        assert!(entry.contains("Icon=nyx-refrain\n"));
        assert!(entry.contains("Terminal=false\n"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(!entry.contains("NoDisplay"));
    }

    #[test]
    fn test_is_desktop_entry_enabled() {
        // Standard newly created entry
        let standard = "[Desktop Entry]\n\
                        Type=Application\n\
                        Name=Nyx Refrain\n\
                        Exec=nyx-refrain --autostart\n\
                        Icon=nyx-refrain\n\
                        Terminal=false\n\
                        X-GNOME-Autostart-enabled=true\n";
        assert!(is_desktop_entry_enabled(standard));

        // Disabled via Hidden=true
        let hidden = "[Desktop Entry]\n\
                      Type=Application\n\
                      Name=Nyx Refrain\n\
                      Hidden=true\n\
                      X-GNOME-Autostart-enabled=true\n";
        assert!(!is_desktop_entry_enabled(hidden));

        // Disabled via X-GNOME-Autostart-enabled=false
        let gnome_disabled = "[Desktop Entry]\n\
                              Type=Application\n\
                              Name=Nyx Refrain\n\
                              X-GNOME-Autostart-enabled=false\n";
        assert!(!is_desktop_entry_enabled(gnome_disabled));

        // Hidden=false is enabled
        let not_hidden = "[Desktop Entry]\n\
                          Type=Application\n\
                          Hidden=false\n\
                          X-GNOME-Autostart-enabled=true\n";
        assert!(is_desktop_entry_enabled(not_hidden));

        // Case insensitivity
        let upper_hidden = "hidden=TRUE\n";
        assert!(!is_desktop_entry_enabled(upper_hidden));
        let upper_gnome = "x-gnome-autostart-enabled=FALSE\n";
        assert!(!is_desktop_entry_enabled(upper_gnome));
    }

    #[test]
    fn test_resolve_linux_exec_cmd() {
        let p = Path::new("/usr/bin/nyx-refrain");
        assert_eq!(resolve_linux_exec_cmd(p, true), "nyx-refrain");

        let p_custom = Path::new("/home/test/bin/nyx-refrain");
        assert_eq!(
            resolve_linux_exec_cmd(p_custom, false),
            "/home/test/bin/nyx-refrain"
        );

        let p_spaces = Path::new("/home/test/My Apps/nyx-refrain");
        assert_eq!(
            resolve_linux_exec_cmd(p_spaces, false),
            r#""/home/test/My Apps/nyx-refrain""#
        );
    }

    #[test]
    fn test_linux_autostart_file_io_in_temp_dir() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nyx_refrain_test_autostart_{unique_id}"));

        // 1. Initially disabled (file does not exist)
        assert_eq!(is_linux_autostart_enabled_in(&temp_dir), Ok(false));

        // 2. Enable autostart
        assert_eq!(
            set_linux_autostart_enabled_in(&temp_dir, true, "nyx-refrain"),
            Ok(())
        );
        let desktop_file = temp_dir.join("nyx-refrain.desktop");
        assert!(desktop_file.is_file());
        assert_eq!(is_linux_autostart_enabled_in(&temp_dir), Ok(true));

        // Verify content
        let content = std::fs::read_to_string(&desktop_file).unwrap();
        assert!(content.contains("Exec=nyx-refrain --autostart"));
        assert!(content.contains("X-GNOME-Autostart-enabled=true"));

        // 3. Mark as Hidden=true manually
        std::fs::write(&desktop_file, format!("{content}Hidden=true\n")).unwrap();
        assert_eq!(is_linux_autostart_enabled_in(&temp_dir), Ok(false));

        // 4. Disable autostart (deletes file)
        assert_eq!(
            set_linux_autostart_enabled_in(&temp_dir, false, "nyx-refrain"),
            Ok(())
        );
        assert!(!desktop_file.exists());
        assert_eq!(is_linux_autostart_enabled_in(&temp_dir), Ok(false));

        // Clean up temp dir
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
