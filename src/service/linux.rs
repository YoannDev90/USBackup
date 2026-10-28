use log::info;
use std::process::Command;

/// Install and enable the systemd user service for the current user.
pub fn install() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // BaseDirs resolves the user home directory.
    let home = directories::BaseDirs::new()
        .ok_or("Could not determine the home directory")?
        .home_dir()
        .to_string_lossy()
        .into_owned();
    let exe_path = std::env::current_exe()?;
    let service_dir = format!("{}/.config/systemd/user", home);
    let service_path = format!("{}/usbackup.service", service_dir);

    if std::path::Path::new(&service_path).exists() {
        println!("Service already installed at {}", service_path);
        return Ok(());
    }

    info!("Installing systemd user service...");
    std::fs::create_dir_all(&service_dir)?;

    let template = include_str!("../../usbackup.service");
    let service_content = template.replace("%EXE_PATH%", &exe_path.display().to_string());

    std::fs::write(&service_path, service_content)?;

    Command::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
        .output()?;
    Command::new("systemctl")
        .arg("--user")
        .arg("enable")
        .arg("usbackup.service")
        .output()?;

    println!("Service installed and enabled. Start it with:");
    println!("  systemctl --user start usbackup.service");
    Ok(())
}
