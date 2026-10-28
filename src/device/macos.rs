//! macOS device backend — stub. `feat/mac-support` replaces this with
//! diskutil/system_profiler based discovery (plan_mac.md M2).
use super::UsbPartition;
use log::debug;
use std::path::Path;

pub fn find_usb_partitions() -> Vec<UsbPartition> {
    debug!("device: macOS backend is a stub, no partitions will be found");
    Vec::new()
}

pub async fn mount_partition(part: &str) -> bool {
    debug!("device: macOS mount is a stub ({} not mounted)", part);
    false
}

pub async fn unmount_by_mount_point(mount_point: &Path) -> bool {
    debug!(
        "device: macOS unmount is a stub ({:?} not unmounted)",
        mount_point
    );
    false
}

/// macOS mounts external volumes under /Volumes.
pub fn is_removable_mount(mount_point: &str) -> bool {
    mount_point.starts_with("/Volumes/")
}

/// sysinfo reports BSD disk names (disk2s1 -> /dev/disk2s1).
pub fn to_devnode(name: &str) -> String {
    format!("/dev/{}", name)
}
