use notify_rust::Notification;

pub fn send_notification(summary: &str, body: &str) {
    let _ = Notification::new()
        .summary(summary)
        .body(body)
        .appname("USBackup")
        .timeout(5000)
        .show();
}

pub fn notify_backup_start(device_name: &str) {
    send_notification(
        "🚀 Backup Started",
        &format!("Starting backup for device: {}", device_name),
    );
}

pub fn notify_backup_success(device_name: &str) {
    send_notification(
        "✅ Backup Complete",
        &format!("Backup successfully finished for: {}", device_name),
    );
}

#[allow(dead_code)]
pub fn notify_backup_error(device_name: &str, error: &str) {
    send_notification(
        "❌ Backup Failed",
        &format!("Error during backup of {}: {}", device_name, error),
    );
}
