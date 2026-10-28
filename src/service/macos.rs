//! launchd backend (plan_mac.md M4): a LaunchAgent plist loaded into the
//! user's `gui` domain, so the agent runs in the interactive session (desktop
//! notifications, /Volumes access) and starts again at every login.
use log::info;
use std::process::Command;

pub fn install() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = directories::BaseDirs::new()
        .ok_or("Could not determine the home directory")?
        .home_dir()
        .to_path_buf();
    let exe_path = std::env::current_exe()?;
    let launch_agents = home.join("Library/LaunchAgents");
    let logs = home.join("Library/Logs");
    let plist_path = launch_agents.join("com.usbackup.agent.plist");

    if plist_path.exists() {
        println!("Service already installed at {}", plist_path.display());
        return Ok(());
    }

    info!("Installing launchd LaunchAgent...");
    std::fs::create_dir_all(&launch_agents)?;
    // launchd will not create the log target itself.
    std::fs::create_dir_all(&logs)?;

    let template = include_str!("../../usbackup.plist");
    let content = template
        .replace("%EXE_PATH%", &exe_path.display().to_string())
        .replace("%LOG_PATH%", &logs.join("usbackup.log").display().to_string());
    std::fs::write(&plist_path, content)?;

    // launchd's gui domain is keyed by the numeric user id.
    let uid_out = Command::new("id").arg("-u").output()?;
    if !uid_out.status.success() {
        return Err("id -u failed".into());
    }
    let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
    let domain = format!("gui/{}", uid);

    let out = Command::new("launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(&plist_path)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("launchctl bootstrap failed: {}", stderr.trim()).into());
    }

    info!("launchd LaunchAgent loaded");
    println!("Service installed. It starts automatically at login.");
    println!("Start it now with: launchctl kickstart {domain}/com.usbackup.agent");
    println!("Remove it with:    launchctl bootout {domain}/com.usbackup.agent");
    Ok(())
}
