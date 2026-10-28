//! Background service installation, one backend per supported OS:
//! systemd user service (linux), Task Scheduler (windows), launchd
//! (macos — stub until plan_mac.md M4 fills it in).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;
