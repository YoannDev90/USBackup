# macOS

Native support: `diskutil` discovery and mounting, `system_profiler` for
the USB ids, launchd LaunchAgent background agent. Releases ship a
universal binary (Apple Silicon + Intel).

## Requirements

- **Nothing to install** — `rsync` and `diskutil` ship with macOS, and
  volumes auto-mount under `/Volumes`.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/YoannDev90/USBackup/master/install.sh | bash
```

The same script as on Linux; it fetches the universal
`usbackup-macos-universal` asset (see the installer options in
[docs/linux.md](linux.md#install)).

- **Gatekeeper** — release binaries are not signed or notarized yet. If
  macOS blocks the downloaded binary, clear the quarantine attribute:
  `xattr -d com.apple.quarantine ~/.local/bin/USBackup`.

## Background service (launchd LaunchAgent, loads at login)

```bash
usbackup --install-service
launchctl kickstart gui/$(id -u)/com.usbackup.agent
```

Remove it with `launchctl bootout gui/$(id -u)/com.usbackup.agent`. The
agent runs in your user session, so desktop notifications and `/Volumes`
access keep working; stdout/stderr go to `~/Library/Logs/usbackup.log`.

## Behaviour

- **Discovery & mounting** — partitions come from
  `diskutil list -plist external physical` (with a per-disk fallback when
  a volume has no UUID) and the USB vendor/product ids from
  `system_profiler SPUSBDataType`; volumes auto-mount under `/Volumes`.
- **Incremental snapshots** — fully supported (APFS hard links).
- **Universal binary** — release builds compile both
  `aarch64-apple-darwin` and `x86_64-apple-darwin` and merge them with
  `lipo`, so one asset runs on every Mac.

## File locations

| What | Path |
| :-- | :-- |
| Local config (secret key + whitelists) | `~/Library/Application Support/com.usbackup.USBackup/backup_config.toml` |
| Pending device queue | `~/Library/Application Support/com.usbackup.USBackup/pending_devices.toml` |
| Per-device rsync logs | `~/Library/Application Support/com.usbackup.USBackup/logs/<name>.log` |
| launchd LaunchAgent | `~/Library/LaunchAgents/com.usbackup.agent.plist` |
| Agent output | `~/Library/Logs/usbackup.log` |
