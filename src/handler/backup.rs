use crate::handler::udev_utils::{find_usb_partitions, mount_partition};
use crate::models::device::DeviceConfig;
use crate::notifications;
use log::{debug, error, info, warn};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use sysinfo::Disks;
use tokio::process::Command as TokioCommand;

pub async fn trigger_backup(device_config: &DeviceConfig) {
    info!("Preparing backup for {}", device_config.name);

    // Disk detection via sysinfo (Cross-platform) with retries
    let mut found_mount_point = None;
    let mut attempts = 0;

    debug!("Monitoring mount (30 attempts)...");

    while attempts < 30 && found_mount_point.is_none() {
        if attempts > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }

        // ATTEMPT MOUNT ON EVERY TRY if not mounted
        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            if mount_partition(&part).await {
                info!("Mount successful for {}", part);
            }
        }

        let disks = Disks::new_with_refreshed_list();

        // Additional debug: show lsblk output during the attempt
        if attempts % 5 == 0 {
            let lsblk_out = TokioCommand::new("lsblk")
                .arg("-o")
                .arg("NAME,TRAN,MOUNTPOINT")
                .output()
                .await;
            if let Ok(out) = lsblk_out {
                debug!("LSBLK Snapshot:\n{}", String::from_utf8_lossy(&out.stdout));
            }
        }

        for disk in &disks {
            let name = disk.name().to_string_lossy();
            let mount_point = disk.mount_point().to_string_lossy();
            let file_system = disk.file_system().to_string_lossy();

            debug!(
                "[Attempt {}] Disk found: {} | Mount: {} | FS: {}",
                attempts + 1,
                name,
                mount_point,
                file_system
            );

            // Detection strategy: UUID first, then name heuristics
            let mut matches = false;

            // Try matching by UUID if available
            if let Some(conf_uuid) = &device_config.uuid {
                // We need to find the devnode for this disk to get its UUID
                // sysinfo doesn't give us the UUID directly easily,
                // so we check partitions found by udev
                let usb_parts = find_usb_partitions();
                for part in usb_parts {
                    if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                        if u == *conf_uuid {
                            // Verify this partition is indeed mounted at mount_point
                            if disk.mount_point().to_string_lossy() == mount_point {
                                matches = true;
                                break;
                            }
                        }
                    }
                }
            }

            if !matches {
                let is_removable =
                    mount_point.contains("/media/") || mount_point.contains("/run/media/");

                if mount_point
                    .to_lowercase()
                    .contains(&device_config.name.to_lowercase())
                    || name
                        .to_lowercase()
                        .contains(&device_config.name.to_lowercase())
                    || is_removable
                {
                    matches = true;
                }
            }

            if matches {
                found_mount_point = Some(disk.mount_point().to_path_buf());
                break;
            }
        }
        attempts += 1;
    }

    if let Some(usb_path) = found_mount_point {
        info!("Disk detected on: {:?}", usb_path);

        notifications::notify_backup_start(&device_config.name);

        let mut has_errors = false;
        for rule in &device_config.backup_rules {
            let full_dest = usb_path.join(rule.destination_path.trim_start_matches('/'));

            info!("Syncing {} to {:?}", rule.source_path, full_dest);

            // Check if source exists
            if !Path::new(&rule.source_path).exists() {
                error!("Source missing: {}", rule.source_path);
                has_errors = true;
                continue;
            }

            // Create destination directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(&full_dest) {
                error!("Unable to create destination {:?}: {}", full_dest, e);
                has_errors = true;
                continue;
            }

            // Use rsync for efficient synchronization
            let mut cmd = TokioCommand::new("rsync");
            cmd.arg("-avz");

            if rule.delete_missing {
                cmd.arg("--delete");
            }

            // Add exclusions
            for pattern in &rule.exclude {
                cmd.arg(format!("--exclude={}", pattern));
            }

            let output = cmd.arg(&rule.source_path).arg(&full_dest).output().await;

            match output {
                Ok(out) if out.status.success() => {
                    info!("Success for {}", rule.source_path);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &String::from_utf8_lossy(&out.stdout),
                    );
                }
                Ok(out) => {
                    let code = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "Unknown".to_string());
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    error!("Rsync failed with code {} for {}", code, rule.source_path);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &format!("FAILED (code {})\n{}", code, stderr),
                    );
                    has_errors = true;
                }
                Err(e) => {
                    error!("Error during rsync execution: {}", e);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &format!("EXECUTION ERROR: {}", e),
                    );
                    has_errors = true;
                }
            }
        }

        if has_errors {
            notifications::notify_backup_error(
                &device_config.name,
                "Certaines synchronisations ont échoué. Vérifiez les logs.",
            );
        } else {
            notifications::notify_backup_success(&device_config.name);
        }
    } else {
        warn!("Unable to locate mount point.");
        notifications::notify_backup_error(
            &device_config.name,
            "Impossible de localiser le point de montage du disque.",
        );
        return;
    }
}

fn append_to_log(device_name: &str, source: &str, content: &str) {
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".local/share/usbackup/logs"))
        .unwrap_or_else(|| Path::new("/tmp/usbackup/logs").to_path_buf());

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        error!("Could not create log directory: {}", e);
        return;
    }

    let log_file = log_dir.join(format!("{}.log", device_name.replace(' ', "_")));
    let mut file = match OpenOptions::new().create(true).append(true).open(&log_file) {
        Ok(f) => f,
        Err(e) => {
            error!("Could not open log file {:?}: {}", log_file, e);
            return;
        }
    };

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let log_entry = format!(
        "\n--- [{}] Source: {} ---\n{}\n---------------------------\n",
        timestamp, source, content
    );

    if let Err(e) = file.write_all(log_entry.as_bytes()) {
        error!("Could not write to log file: {}", e);
    }
}
