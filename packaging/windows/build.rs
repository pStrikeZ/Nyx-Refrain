//! Shared build script for the GUI and CLI packages: the `NYX_VERSION` string on every
//! target, plus on Windows the icon and the VERSIONINFO block shown by Explorer (Details
//! tab) and Task Manager (name, publisher).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/windows/build.rs");
    println!("cargo:rustc-env=NYX_VERSION={}", build_version());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let package = env::var("CARGO_PKG_NAME").unwrap();
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join(format!("../../assets/icons/windows/{package}.ico"));
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon = icon.canonicalize().expect("Windows icon must exist");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let rc = output.join("icon.rc");
    let resource = output.join("icon.res");
    // UTF-8 source (for the copyright sign). Forward slashes also work on Windows and avoid
    // RC string escape sequences.
    let mut source = String::from("#pragma code_page(65001)\n");
    source.push_str(&format!(
        "1 ICON \"{}\"\n",
        icon.to_string_lossy().replace('\\', "/")
    ));
    source.push_str(&version_info(&package));
    fs::write(&rc, source).expect("write resource source");
    let status = Command::new("zig")
        .args(["rc", "/fo"])
        .arg(&resource)
        .arg("--")
        .arg(&rc)
        .status()
        .expect("Windows resources require zig on PATH");
    assert!(status.success(), "zig rc failed for {package}");
    println!("cargo:rustc-link-arg-bin={package}={}", resource.display());
}

/// `v0.9.0` when HEAD is the clean `v0.9.0` tag, otherwise `v0.9.0-g1a2b3c4` (plus `-dirty`
/// for uncommitted changes to tracked files). Without git (e.g. a source tarball) it is the
/// plain `v0.9.0`.
fn build_version() -> String {
    let version = format!("v{}", env::var("CARGO_PKG_VERSION").unwrap());
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) else {
        return version;
    };
    // HEAD moves on commit / checkout; the index changes whenever tracked files are staged or
    // their stat info is refreshed, which is close enough for the dirty flag. Only existing
    // paths are watched: cargo treats a missing rerun-if-changed path as always stale, and
    // `packed-refs` often does not exist. A packed branch ref has no loose file until the next
    // commit creates one, so `refs/heads` is watched instead.
    let head_ref = git(&["symbolic-ref", "-q", "HEAD"])
        .filter(|r| Path::new(&format!("{git_dir}/{r}")).exists())
        .unwrap_or_else(|| "refs/heads".to_string());
    for f in ["HEAD", "index", "packed-refs", "refs/tags", &head_ref] {
        let path = format!("{git_dir}/{f}");
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let Some(hash) = git(&["rev-parse", "--short=7", "HEAD"]) else {
        return version;
    };
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let tagged =
        git(&["tag", "--points-at", "HEAD"]).is_some_and(|tags| tags.lines().any(|t| t == version));
    match (tagged, dirty) {
        (true, false) => version,
        (_, false) => format!("{version}-g{hash}"),
        (_, true) => format!("{version}-g{hash}-dirty"),
    }
}

/// VERSIONINFO resource. Task Manager shows FileDescription as the app name (also in the
/// Startup apps list) and CompanyName as the publisher.
fn version_info(package: &str) -> String {
    let (product, description, comments, file) = match package {
        "nyx-refrain" => (
            "Nyx Refrain (GUI)",
            "Nyx Refrain",
            "Stream system audio to AirPlay speakers with low latency",
            "nyx-refrain.exe",
        ),
        "nyxr" => (
            "Nyx Refrain (CLI)",
            "Nyx Refrain CLI",
            "Command-line AirPlay audio streamer",
            "nyxr.exe",
        ),
        other => panic!("no Windows version info defined for package {other}"),
    };
    let publisher = env::var("CARGO_PKG_AUTHORS").unwrap_or_default();
    assert!(
        !publisher.is_empty(),
        "{package}: set `authors.workspace = true` (VERSIONINFO publisher)"
    );
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let num = |key: &str| env::var(key).unwrap_or_else(|_| "0".into());
    let (major, minor, patch) = (
        num("CARGO_PKG_VERSION_MAJOR"),
        num("CARGO_PKG_VERSION_MINOR"),
        num("CARGO_PKG_VERSION_PATCH"),
    );
    format!(
        r#"1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "{publisher}"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{file}"
            VALUE "LegalCopyright", "Copyright © 2026 {publisher}"
            VALUE "OriginalFilename", "{file}"
            VALUE "ProductName", "{product}"
            VALUE "ProductVersion", "{version}"
            VALUE "Comments", "{comments}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
}
