# Windows

Native support: WMI discovery, auto-mount handled by the OS itself, Task
Scheduler background agent. Releases ship x64 and arm64 `.exe`.

## Requirements

- **rsync** — a native MSVC build such as
  [nuket/rsync-windows](https://github.com/nuket/rsync-windows)
  (x64 / arm64), present on `PATH` as `rsync`.
- Nothing else: Windows mounts removable drives by itself, so there is no
  `udisksctl` equivalent to install.

## Install

```powershell
irm https://raw.githubusercontent.com/YoannDev90/USBackup/master/install.ps1 | iex
```

The script resolves the latest release, verifies its SHA256 checksum,
installs the binary to `%LOCALAPPDATA%\Programs\USBackup`, and offers to
register the scheduled task — the same flow as `install.sh`. Saved to
disk it also accepts `-Prefix`, `-Version`, `-NoService` and `-Force`.

## Background service (Task Scheduler, runs at logon)

```powershell
USBackup.exe --install-service
schtasks /Run /TN USBackup
```

The task runs in your interactive session (not Session 0), so toast
notifications and `%APPDATA%` access keep working.

## Behaviour & quirks

- **Discovery & mounting** — devices come from WMI (`Win32_DiskDrive`
  joined to `Win32_LogicalDisk` by volume serial); Windows mounts
  removable drives itself, so there is no `udisksctl` step.
- **Incremental snapshots are off** — the native
  [nuket/rsync-windows](https://github.com/nuket/rsync-windows) port
  cannot create the hard links `--link-dest` relies on, so `incremental`
  is forced to `false` at backup time (a warning is logged when a key
  asks for it) and every backup runs as a plain mirror: no `latest` link,
  no snapshot history. Linux and macOS keep both.
- **SmartScreen** — release `.exe` files are not code-signed yet;
  SmartScreen may prompt on first launch ("More info" → "Run anyway").

## File locations

| What | Path |
| :-- | :-- |
| Local config (secret key + whitelists) | `%APPDATA%\usbackup\USBackup\config\backup_config.toml` |
| Pending device queue | `%APPDATA%\usbackup\USBackup\config\pending_devices.toml` |
| Per-device rsync logs | `%APPDATA%\usbackup\USBackup\data\logs\<name>.log` |
| Scheduled task | Task Scheduler task `USBackup` (`schtasks /Query /TN USBackup`) |
