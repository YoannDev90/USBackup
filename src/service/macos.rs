//! launchd backend — stub. plan_mac.md M4 replaces this with the
//! LaunchAgent plist (bootstrap/kickstart/bootout).
use log::debug;

pub fn install() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    debug!("service: macOS launchd install is not implemented yet");
    Err("macOS service installation is not implemented yet".into())
}
