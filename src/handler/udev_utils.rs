use log::{debug, error, info};
use std::fs;
use std::path::Path;
use tokio::process::Command as TokioCommand;

pub fn find_usb_partitions() -> Vec<String> {
    let mut partitions = Vec::new();
    let mut enumerator = match udev::Enumerator::new() {
        Ok(e) => e,
        Err(_) => return partitions,
    };

    let _ = enumerator.match_subsystem("block");
    let _ = enumerator.match_property("DEVTYPE", "partition");

    if let Ok(devices) = enumerator.scan_devices() {
        for device in devices {
            if let Some(devnode) = device.devnode() {
                debug!("udev examining: {:?}", devnode);
            }
            let mut current = Some(device.clone());
            let mut is_usb = false;
            while let Some(parent) = current {
                if let Some(bus) = parent.property_value("ID_BUS") {
                    if bus == "usb" {
                        is_usb = true;
                        break;
                    }
                }
                current = parent.parent();
            }

            if is_usb {
                if let Some(devnode) = device.devnode() {
                    debug!("USB Partition found via udev: {:?}", devnode);
                    partitions.push(devnode.to_string_lossy().to_string());
                }
            }
        }
    }
    partitions
}

pub fn get_partition_uuid(devnode: &str) -> Option<String> {
    let mut enumerator = udev::Enumerator::new().ok()?;
    enumerator.match_subsystem("block").ok()?;

    let devices = enumerator.scan_devices().ok()?;
    for device in devices {
        if let Some(node) = device.devnode() {
            if node.to_string_lossy() == devnode {
                return device
                    .property_value("ID_FS_UUID")
                    .map(|v| v.to_string_lossy().to_string());
            }
        }
    }
    None
}

pub async fn mount_partition(part: &str) -> bool {
    let output = TokioCommand::new("udisksctl")
        .arg("mount")
        .arg("-b")
        .arg(part)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            debug!("Successfully mounted partition {}", part);
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // On ne log en error que si ce n'est pas "already mounted" car c'est un cas fréquent et normal
            if stderr.contains("already mounted") {
                debug!("Partition {} is already mounted.", part);
                true
            } else {
                debug!("udisksctl mount failed for {}: {}", part, stderr);
                false
            }
        }
        Err(e) => {
            debug!("Failed to execute udisksctl: {}", e);
            false
        }
    }
}

/// Resolve the block device node backing a mount point by parsing /proc/self/mounts.
/// `udisksctl unmount -b` expects a device node (e.g. /dev/sdb1), not a mount point.
pub fn find_devnode_for_mount(mount_point: &Path) -> Option<String> {
    let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
    let target = mount_point.to_string_lossy();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let devnode = parts.next()?;
        let mount = parts.next()?;
        if mount == target {
            return Some(devnode.to_string());
        }
    }
    None
}

/// Unmount a device given its mount point. Resolves the real block device
/// first because `udisksctl unmount -b` takes a device node, not a path.
pub async fn unmount_by_mount_point(mount_point: &Path) -> bool {
    let devnode = match find_devnode_for_mount(mount_point) {
        Some(d) => d,
        None => {
            error!(
                "Unable to resolve a block device for mount point {:?}",
                mount_point
            );
            return false;
        }
    };

    let output = TokioCommand::new("udisksctl")
        .arg("unmount")
        .arg("-b")
        .arg(&devnode)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            info!("Successfully unmounted {}", devnode);
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not mounted") {
                debug!("{} is already unmounted.", devnode);
                true
            } else {
                error!("udisksctl unmount failed for {}: {}", devnode, stderr);
                false
            }
        }
        Err(e) => {
            error!("Failed to execute udisksctl unmount for {}: {}", devnode, e);
            false
        }
    }
}
