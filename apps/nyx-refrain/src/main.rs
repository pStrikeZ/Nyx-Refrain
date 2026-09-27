//! Nyx Refrain tray GUI front-end entry point.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod engine;
mod i18n;
mod icons;
mod settings;
mod single_instance;
#[cfg(any(windows, test))]
mod soft_render;
pub mod tray_menu;

#[cfg(windows)]
mod position;
#[cfg(windows)]
mod ui_windows;

#[cfg(target_os = "linux")]
mod ui_linux;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    ui_windows::run()
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    ui_linux::run()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("nyx-refrain: platform not supported");
}
