//! Windows device backend — stub. `feat/win-support` replaces this with
//! WMI based discovery (plan_windows.md M3).
use super::UsbPartition;
use log::debug;
use std::path::Path;

pub fn find_usb_partitions() -> Vec<UsbPartition> {
    debug!("device: Windows backend is a stub, no partitions will be found");
    Vec::new()
}

pub async fn mount_partition(part: &str) -> bool {
    // Windows auto-mounts volumes on drive letters; nothing to do.
    debug!("device: Windows mount is a stub ({} auto-mounted by the OS)", part);
    true
}

pub fn find_devnode_for_mount(mount_point: &Path) -> Option<String> {
    debug!(
        "device: Windows find_devnode_for_mount is a stub ({:?})",
        mount_point
    );
    None
}

pub async fn unmount_by_mount_point(mount_point: &Path) -> bool {
    debug!(
        "device: Windows unmount is a stub ({:?} not unmounted)",
        mount_point
    );
    false
}
