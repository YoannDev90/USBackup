use super::UsbPartition;
use log::{debug, error, info};
use std::fs;
use std::path::Path;
use tokio::process::Command as TokioCommand;

fn parse_hex_u16(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim(), 16).ok()
}

/// Extract VID/PID for a partition: prefer ID_VENDOR_ID/ID_MODEL_ID
/// properties, fall back to walking up to the USB parent device's
/// idVendor/idProduct sysfs attributes.
fn usb_ids_for_partition(device: &udev::Device) -> (Option<u16>, Option<u16>) {
    let vid = device
        .property_value("ID_VENDOR_ID")
        .and_then(|v| parse_hex_u16(&v.to_string_lossy()));
    let pid = device
        .property_value("ID_MODEL_ID")
        .and_then(|v| parse_hex_u16(&v.to_string_lossy()));
    if vid.is_some() && pid.is_some() {
        return (vid, pid);
    }

    let mut current = device.parent();
    while let Some(parent) = current {
        if let Some(v) = parent.attribute_value("idVendor") {
            let vid = parse_hex_u16(&v.to_string_lossy());
            let pid = parent
                .attribute_value("idProduct")
                .and_then(|p| parse_hex_u16(&p.to_string_lossy()));
            return (vid, pid);
        }
        current = parent.parent();
    }
    (None, None)
}

pub fn find_usb_partitions() -> Vec<UsbPartition> {
    let mut partitions = Vec::new();
    let mut enumerator = match udev::Enumerator::new() {
        Ok(e) => e,
        Err(_) => return partitions,
    };

    let _ = enumerator.match_subsystem("block");
    let _ = enumerator.match_property("DEVTYPE", "partition");

    if let Ok(devices) = enumerator.scan_devices() {
        for device in devices {
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

            if !is_usb {
                continue;
            }
            if let Some(devnode) = device.devnode() {
                let (vendor_id, product_id) = usb_ids_for_partition(&device);
                let uuid = device
                    .property_value("ID_FS_UUID")
                    .map(|v| v.to_string_lossy().to_string());
                let part = UsbPartition {
                    devnode: devnode.to_string_lossy().to_string(),
                    uuid,
                    vendor_id,
                    product_id,
                };
                debug!(
                    "udev: {} bus=usb uuid={:?} id_vendor={:04x?} id_model={:04x?} fs_type={:?}",
                    part.devnode,
                    part.uuid,
                    part.vendor_id,
                    part.product_id,
                    device
                        .property_value("ID_FS_TYPE")
                        .map(|v| v.to_string_lossy().to_string())
                );
                partitions.push(part);
            }
        }
    }
    partitions
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
            // Log at debug when already mounted: frequent and normal
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

/// Whether a mount point hosts removable media. Linux desktops mount
/// USB volumes under /media or /run/media.
pub fn is_removable_mount(mount_point: &str) -> bool {
    mount_point.starts_with("/run/media/") || mount_point.starts_with("/media/")
}

/// Device node for a disk name as reported by sysinfo (sdb1 -> /dev/sdb1).
pub fn to_devnode(name: &str) -> String {
    format!("/dev/{}", name)
}
