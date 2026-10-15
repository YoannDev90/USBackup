use nusb::hotplug::HotplugEvent;

mod handler;
mod models;
mod notifications;
mod storage;

use crate::storage::{load_config, PendingDevice};
use log::{debug, error, info, warn};
use std::io::IsTerminal;
use std::process::Command;

const USAGE: &str = "\
USBackup - automated USB backup agent

Usage:
  usbackup                      Start the background agent (daemon)
  usbackup --help               Show this help
  usbackup --version            Show version
  usbackup --install-service    Install and enable the systemd user service
  usbackup approve <uuid> [--configure]
                                Approve a pending device; --configure also
                                runs the interactive backup wizard
  usbackup list-devices         List approved / ask-each-time / ignored /
                                pending devices

In a terminal the agent prompts interactively for unknown devices.
Under systemd (no TTY) it stores them as pending and sends a desktop
notification; approve them with `usbackup approve <uuid>`.";
fn check_dependencies() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let deps = ["rsync", "udisksctl"];
    for dep in deps {
        if !Command::new("which").arg(dep).output()?.status.success() {
            error!("Missing dependency: {}", dep);
            return Err(format!("The tool '{}' is required but is not installed.", dep).into());
        }
    }
    Ok(())
}

fn install_systemd_service() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = std::env::var("HOME")?;
    let exe_path = std::env::current_exe()?;
    let service_dir = format!("{}/.config/systemd/user", home);
    let service_path = format!("{}/usbackup.service", service_dir);

    if std::path::Path::new(&service_path).exists() {
        println!("Service already installed at {}", service_path);
        return Ok(());
    }

    info!("Installing systemd user service...");
    std::fs::create_dir_all(&service_dir)?;

    let template = include_str!("../usbackup.service");
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

fn cmd_list_devices() {
    let config = load_config();
    println!("Whitelist (automatic backup):");
    for u in &config.approved_uuids {
        println!("  {}", u);
    }
    println!("AskEachTime (prompt on plug-in):");
    for u in &config.ask_each_time_uuids {
        println!("  {}", u);
    }
    println!("IgnoreForever:");
    for u in &config.ignored_uuids {
        println!("  {}", u);
    }
    println!("Pending approval:");
    for p in storage::load_pending() {
        println!(
            "  {}  \"{}\"  (vid={:04x} pid={:04x})",
            p.uuid, p.product, p.vendor_id, p.product_id
        );
    }
}

async fn cmd_approve(
    args: &[String],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let uuid = args
        .first()
        .ok_or("usage: usbackup approve <uuid> [--configure]")?;
    let configure = args.iter().any(|a| a == "--configure");

    let pending = storage::load_pending();
    let device = pending.iter().find(|p| p.uuid == *uuid).cloned();

    let mut config = load_config();
    if !config.approved_uuids.contains(uuid) {
        // Move across lists if it was previously ignored / ask-each-time
        config.ask_each_time_uuids.retain(|u| u != uuid);
        config.ignored_uuids.retain(|u| u != uuid);
        config.approved_uuids.push(uuid.clone());
        storage::save_config(&config)?;
    }
    storage::remove_pending(uuid);
    println!("Device {} approved (Whitelist).", uuid);

    if configure {
        match device {
            Some(dev) => {
                println!("Running configuration wizard for \"{}\"...", dev.product);
                handler::run_wizard(dev.vendor_id, dev.product_id, &dev.product, uuid).await?;
            }
            None => {
                eprintln!(
                    "No pending record for {}; cannot run the wizard (VID/PID unknown).\n\
                     Plug the device in again, or approve without --configure.",
                    uuid
                );
            }
        }
    }
    Ok(())
}

async fn handle_new_device_interactive(
    vid: u16,
    pid: u16,
    product: &str,
    uuid: &str,
) {
    let options = [
        "Approve and configure backups (Whitelist)",
        "Approve, but ask every time (AskEachTime)",
        "Ignore this device forever (IgnoreForever)",
        "Cancel",
    ];
    let selection = dialoguer::Select::new()
        .with_prompt(format!(
            "New device '{}' detected ({}). Choose an action:",
            product, uuid
        ))
        .items(&options)
        .default(0)
        .interact()
        .unwrap_or(3);

    let mut config = load_config();
    match selection {
        0 => {
            if !config.approved_uuids.contains(&uuid.to_string()) {
                config.approved_uuids.push(uuid.to_string());
                if let Err(e) = storage::save_config(&config) {
                    error!("Failed to save config: {}", e);
                }
            }

            if let Err(e) = handler::run_wizard(vid, pid, product, uuid).await {
                error!("Wizard error: {}", e);
            }
        }
        1 => {
            if !config.ask_each_time_uuids.contains(&uuid.to_string()) {
                config.ask_each_time_uuids.push(uuid.to_string());
                if let Err(e) = storage::save_config(&config) {
                    error!("Failed to save config: {}", e);
                }
            }
            info!("Device {} set to AskEachTime.", uuid);
        }
        2 => {
            if !config.ignored_uuids.contains(&uuid.to_string()) {
                config.ignored_uuids.push(uuid.to_string());
                if let Err(e) = storage::save_config(&config) {
                    error!("Failed to save config: {}", e);
                }
            }
            info!("Device {} set to IgnoreForever.", uuid);
        }
        _ => {
            debug!("Device {} approval cancelled.", uuid);
        }
    }
}

fn handle_new_device_noninteractive(vid: u16, pid: u16, product: &str, uuid: &str) {
    storage::add_pending(PendingDevice {
        uuid: uuid.to_string(),
        vendor_id: vid,
        product_id: pid,
        product: product.to_string(),
    });
    warn!(
        "No TTY available; device {} ({}) recorded as pending approval.",
        product, uuid
    );
    notifications::send_notification(
        "New USB device detected",
        &format!(
            "'{}' is not approved. Run: usbackup approve {}",
            product, uuid
        ),
    );
}

async fn run_daemon() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    check_dependencies()?;
    // Note: systemd service install is now an explicit --install-service flag.

    info!("USBackup service started. Waiting for devices...");

    let watch = match nusb::watch_devices() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Initialization error: {}", e);
            return Ok(());
        }
    };

    use futures_lite::StreamExt;
    let mut stream = watch;

    while let Some(event) = stream.next().await {
        match event {
            HotplugEvent::Connected(device) => {
                let vid = device.vendor_id();
                let pid = device.product_id();
                let product = device.product_string().unwrap_or("Unknown").to_string();
                let device_key = format!("{:04x}:{:04x}", vid, pid);

                info!("[+] Device detected: {} ({})", product, device_key);

                // Reload the approved-UUID config
                let config = load_config();

                // Find this device's UUID, correlating by VID/PID so we never
                // pick up a different plugged-in key.
                let mut device_uuid = None;
                for i in 0..5 {
                    if i > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    }

                    let parts = crate::handler::udev_utils::find_usb_partitions();
                    debug!("Checking USB partitions: {:?}", parts);

                    for part in parts {
                        let ids_match = part.vendor_id == Some(vid) && part.product_id == Some(pid);
                        if ids_match {
                            if let Some(u) = part.uuid {
                                device_uuid = Some(u);
                                break;
                            }
                        }
                    }
                    if device_uuid.is_some() {
                        break;
                    }
                }

                if let Some(uuid) = device_uuid {
                    if config.ignored_uuids.contains(&uuid) {
                        debug!("Device {} is set to IgnoreForever; skipping.", uuid);
                        continue;
                    }
                    if config.approved_uuids.contains(&uuid) {
                        // Whitelist: run the backup automatically
                        tokio::spawn(async move {
                            // Wait for the key to mount so its local config can be read
                            if let Some(dev_conf) =
                                crate::handler::trigger_backup_by_uuid(vid, pid, &uuid).await
                            {
                                info!("Backup finished for {}", dev_conf.name);
                            }
                        });
                    } else if config.ask_each_time_uuids.contains(&uuid) {
                        // AskEachTime: prompt on every plug-in (TTY only)
                        if std::io::stdin().is_terminal() {
                            let product_clone = product.clone();
                            tokio::task::spawn_blocking(move || {
                                let run_now = dialoguer::Confirm::new()
                                    .with_prompt(format!(
                                        "Device '{}' ({}) is set to AskEachTime. Run a backup now?",
                                        product_clone, uuid
                                    ))
                                    .default(true)
                                    .interact()
                                    .unwrap_or(false);
                                if run_now {
                                    tokio::spawn(async move {
                                        if let Some(dev_conf) =
                                            crate::handler::trigger_backup_by_uuid(
                                                vid, pid, &uuid,
                                            )
                                            .await
                                        {
                                            info!("Backup finished for {}", dev_conf.name);
                                        }
                                    });
                                }
                            });
                        } else {
                            warn!(
                                "Device {} is AskEachTime but no TTY is available; skipping backup.",
                                uuid
                            );
                            notifications::send_notification(
                                "USBackup",
                                &format!(
                                    "'{}' is AskEachTime; open a terminal to approve a backup.",
                                    product
                                ),
                            );
                        }
                    } else if std::io::stdin().is_terminal() {
                        // New device with a TTY: interactive Select menu
                        let product_clone = product.clone();
                        let uuid_clone = uuid.clone();
                        tokio::spawn(async move {
                            handle_new_device_interactive(
                                vid,
                                pid,
                                &product_clone,
                                &uuid_clone,
                            )
                            .await;
                        });
                    } else {
                        // New device without a TTY: record as pending + notify
                        handle_new_device_noninteractive(vid, pid, &product, &uuid);
                    }
                } else {
                    warn!("Unable to extract UUID for device {}.", product);
                }
            }
            HotplugEvent::Disconnected(_) => {
                info!("[-] Device disconnected");
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--help") | Some("-h") => {
            println!("{}", USAGE);
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("USBackup {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--install-service") => install_systemd_service(),
        Some("approve") => cmd_approve(&args[1..]).await,
        Some("list-devices") => {
            cmd_list_devices();
            Ok(())
        }
        Some(other) => {
            eprintln!("Unknown argument: {}\n", other);
            eprintln!("{}", USAGE);
            std::process::exit(2);
        }
        None => run_daemon().await,
    }
}
