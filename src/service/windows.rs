//! Task Scheduler backend (plan_windows.md §7): a logon-triggered task
//! running in the interactive user session, so toast notifications and
//! %APPDATA% access keep working (unlike a Session 0 Windows service).
use log::info;

pub fn install() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let exe_path = std::env::current_exe()?;
    let exe = exe_path
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    let xml = include_str!("../../usbackup.xml").replace("%EXE_PATH%", &exe);

    // schtasks /XML reads the task definition from a file.
    let xml_path = std::env::temp_dir().join("usbackup-task.xml");
    std::fs::write(&xml_path, &xml)?;

    let out = std::process::Command::new("schtasks")
        .args(["/Create", "/TN", "USBackup", "/XML"])
        .arg(&xml_path)
        .arg("/F")
        .output()?;

    let _ = std::fs::remove_file(&xml_path);

    if out.status.success() {
        info!("Task Scheduler task 'USBackup' installed");
        println!("Service installed. It starts automatically at logon.");
        println!("Start it now with: schtasks /Run /TN USBackup");
        println!("Remove it with:    schtasks /Delete /TN USBackup /F");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("schtasks failed: {}", stderr.trim()).into())
    }
}
