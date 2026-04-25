use crate::handler::udev_utils::{find_usb_partitions, mount_partition};
use crate::models::device::DeviceConfig;
use crate::notifications;
use log::{debug, error, info, warn};
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

            // Improved detection strategy
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
                found_mount_point = Some(disk.mount_point().to_path_buf());
                break;
            }
        }
        attempts += 1;
    }

    if let Some(usb_path) = found_mount_point {
        info!("Disk detected on: {:?}", usb_path);

        notifications::notify_backup_start(&device_config.name);

        for rule in &device_config.backup_rules {
            let full_dest = usb_path.join(rule.destination_path.trim_start_matches('/'));

            info!("Syncing {} to {:?}", rule.source_path, full_dest);

            // Check if source exists
            if !Path::new(&rule.source_path).exists() {
                error!("Source missing: {}", rule.source_path);
                continue;
            }

            // Create destination directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(&full_dest) {
                error!("Unable to create destination {:?}: {}", full_dest, e);
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

            let status = cmd.arg(&rule.source_path).arg(&full_dest).status().await;

            match status {
                Ok(s) if s.success() => info!("Success for {}", rule.source_path),
                Ok(s) => error!("Rsync failed with code {} for {}", s, rule.source_path),
                Err(e) => error!("Error during rsync execution: {}", e),
            }
        }
    } else {
        warn!("Unable to locate mount point.");
        return;
    }

    notifications::notify_backup_success(&device_config.name);
}
