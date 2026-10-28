//! Windows device backend: WMI enumeration over Win32_DiskDrive and
//! Win32_LogicalDisk (plan_windows.md M3).
use super::UsbPartition;
use log::debug;
use std::collections::HashMap;
use std::path::Path;
use wmi::{COMLibrary, Variant, WMIConnection};

fn variant_str(v: &Variant) -> Option<&str> {
    match v {
        Variant::String(s) => Some(s),
        _ => None,
    }
}

fn variant_u32(v: &Variant) -> Option<u32> {
    match v {
        Variant::UI4(n) => Some(*n),
        Variant::I4(n) => u32::try_from(*n).ok(),
        Variant::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Extract `VID_xxxx` / `PID_xxxx` from a PnP device id such as
/// `USB\VID_0781&PID_5591\20061234567`.
fn vid_pid_from_pnp(pnp_id: &str) -> (Option<u16>, Option<u16>) {
    let upper = pnp_id.to_ascii_uppercase();
    (hex_after(&upper, "VID_"), hex_after(&upper, "PID_"))
}

fn hex_after(s: &str, marker: &str) -> Option<u16> {
    let rest = &s[s.find(marker)? + marker.len()..];
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    u16::from_str_radix(&digits, 16).ok()
}

/// `USBSTOR\DISK&VEN_...\<serial>&0` device ids carry no VID/PID: match
/// the disk's instance serial against the USB device instances
/// (`USB\VID_xxxx&PID_xxxx\<serial>`) to recover the ids the hotplug
/// event reports.
fn vid_pid_from_usb_parent(wmi: &WMIConnection, pnp_id: &str) -> (Option<u16>, Option<u16>) {
    let disk_instance = pnp_id.rsplit('\\').next().unwrap_or_default();
    if disk_instance.is_empty() {
        return (None, None);
    }

    let entities: Vec<HashMap<String, Variant>> = match wmi.raw_query(
        "SELECT DeviceID FROM Win32_PnPEntity WHERE DeviceID LIKE 'USB\\VID_%'",
    ) {
        Ok(entities) => entities,
        Err(e) => {
            debug!("device: Win32_PnPEntity query failed: {}", e);
            return (None, None);
        }
    };

    for entity in entities {
        let Some(dev_id) = entity.get("DeviceID").and_then(variant_str) else {
            continue;
        };
        let usb_instance = dev_id.rsplit('\\').next().unwrap_or_default();
        if usb_instance.is_empty() {
            continue;
        }
        // The USBSTOR instance is "<serial>&0" or "<serial>_...": the
        // instance must start with the *whole* USB serial, not a prefix of it.
        if let Some(rest) = disk_instance.strip_prefix(usb_instance) {
            if rest.is_empty() || rest.starts_with('&') || rest.starts_with('_') {
                debug!("device: matched {} to USB device {}", pnp_id, dev_id);
                return vid_pid_from_pnp(dev_id);
            }
        }
    }
    (None, None)
}

pub fn find_usb_partitions() -> Vec<UsbPartition> {
    let mut partitions = Vec::new();

    let com = match COMLibrary::new() {
        Ok(com) => com,
        Err(e) => {
            debug!("device: COM initialization failed: {}", e);
            return partitions;
        }
    };
    let wmi = match WMIConnection::new(com) {
        Ok(wmi) => wmi,
        Err(e) => {
            debug!("device: WMI connection failed: {}", e);
            return partitions;
        }
    };

    // Physical USB disks: InterfaceType = "USB" is the counterpart of the
    // linux ID_BUS=usb udev property.
    let disks: Vec<HashMap<String, Variant>> = match wmi.raw_query(
        "SELECT Index, PNPDeviceID, Model FROM Win32_DiskDrive WHERE InterfaceType = 'USB'",
    ) {
        Ok(disks) => disks,
        Err(e) => {
            debug!("device: Win32_DiskDrive query failed: {}", e);
            return partitions;
        }
    };

    // Logical volumes; DiskIndex joins each volume back to its disk.
    let volumes: Vec<HashMap<String, Variant>> = match wmi
        .raw_query("SELECT DeviceID, VolumeSerialNumber, DiskIndex FROM Win32_LogicalDisk")
    {
        Ok(volumes) => volumes,
        Err(e) => {
            debug!("device: Win32_LogicalDisk query failed: {}", e);
            return partitions;
        }
    };

    for volume in volumes {
        let Some(devnode) = volume.get("DeviceID").and_then(variant_str) else {
            continue;
        };
        let Some(disk_index) = volume.get("DiskIndex").and_then(variant_u32) else {
            continue;
        };
        let Some(disk) = disks
            .iter()
            .find(|d| d.get("Index").and_then(variant_u32) == Some(disk_index))
        else {
            continue;
        };
        let pnp_id = disk
            .get("PNPDeviceID")
            .and_then(variant_str)
            .unwrap_or_default();

        let (vendor_id, product_id) = match vid_pid_from_pnp(pnp_id) {
            ids @ (Some(_), Some(_)) => ids,
            _ => vid_pid_from_usb_parent(&wmi, pnp_id),
        };

        let part = UsbPartition {
            devnode: devnode.to_string(),
            uuid: volume
                .get("VolumeSerialNumber")
                .and_then(variant_str)
                .map(str::to_string),
            vendor_id,
            product_id,
        };
        debug!(
            "wmi: {} serial={:?} id_vendor={:04x?} id_model={:04x?} model={:?} pnp={}",
            part.devnode,
            part.uuid,
            part.vendor_id,
            part.product_id,
            disk.get("Model").and_then(variant_str).unwrap_or("?"),
            pnp_id
        );
        partitions.push(part);
    }

    partitions
}

pub async fn mount_partition(part: &str) -> bool {
    // Windows auto-mounts volumes on drive letters; nothing to do.
    debug!(
        "device: Windows mount: {} is auto-mounted by the OS",
        part
    );
    true
}

/// Not implemented yet: needs a per-drive query (WMI DriveType against
/// the mount's drive letter, or GetDriveTypeW).
pub fn is_removable_mount(_mount_point: &str) -> bool {
    false
}

/// sysinfo reports drive letters ("D:"), which need no /dev prefix.
pub fn to_devnode(name: &str) -> String {
    name.to_string()
}

pub async fn unmount_by_mount_point(mount_point: &Path) -> bool {
    debug!(
        "device: Windows unmount is not implemented yet ({:?} stays mounted)",
        mount_point
    );
    false
}
