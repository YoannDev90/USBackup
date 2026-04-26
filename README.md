# USBackup 🚀

Automated USB backup agent written in Rust. This program runs 24/7 and instantly detects USB device connections to trigger predefined actions.

[Version Française ici (French Version)](README_FR.md)

## ✨ Features

- **Real-time Monitoring**: Detects connect/disconnect events without polling (using native APIs via `nusb`).
- **Standard XDG Paths**: Configuration and logs now follow Linux standards (`~/.config/usbackup` and `~/.local/share/usbackup`).
- **Incremental Snapshots**: Supports time-based versioning using `rsync` hard links (Space efficient versioning).
- **Post-Backup Actions**: Automatically unmount devices or run custom scripts after successful backups.
- **Decentralized Configuration**: Configuration is stored directly on the USB devices (`.usbackup.toml`), making it portable.
- **HMAC Security**: Configurations are cryptographically signed using a local secret key to prevent unauthorized execution.
- **Smart Auto-mount**: Automatically finds and mounts USB partitions using `udev` and `udisksctl`.
- **Interactive Whitelisting**: When a new device is detected, the app asks whether to remember it, ignore it, or ask again later.
- **Background Agent**: Multi-threaded architecture to keep the task responsive during backups.
- **System Notifications**: Uses native desktop notifications to keep you informed.

## �️ How it Works (Technical)

USBackup uses a **Decentralized Configuration** model with a **Zero-Trust** security approach:

1. **Detection**: Listens to `udev` events via `nusb` for instant device detection (no polling).
2. **Identification**: Uses the partition **UUID** to distinguish between different USB devices.
3. **HMAC Signature**: 
   - A unique `secret_key` is generated on your machine.
   - Each device config (`.usbackup.toml`) is signed with this secret using **HMAC-SHA256**.
   - This prevents malicious users from injecting their own configuration to steal files.
4. **Execution**:
   - **Mirror Mode**: Simple one-way synchronization.
   - **Incremental Snapshots**: Using `rsync --link-dest` to create time-stamped versions with minimal disk usage.
   - **Post-Backup**: Automatic unmount or custom script execution.
   - **Smart Exclusions**: Automatically respects your project's `.gitignore` rules.

## �💻 Supported OS

| OS          | Status          | Notes                                              |
| :---------- | :-------------- | :------------------------------------------------- |
| **Linux**   | ✅ Supported     | Native support for udev and auto-mount.            |
| **Windows** | ❌ Not Supported | If you want Windows support, please **Fork + PR**. |
| **macOS**   | ❌ Not Supported | If you want macOS support, please **Fork + PR**.   |

## 🛠️ Installation

### Dependencies (Linux)
You need `libudev` development files installed on your system:
- **Fedora/RHEL**: `sudo dnf install libudev-devel`
- **Ubuntu/Debian**: `sudo apt install libudev-dev pkg-config`

### Build
1. Ensure you have [Rust](https://www.rust-lang.org/) installed.
2. Clone the repository.
3. Build and run:
   ```bash
   cargo run
   ```

## ⚙️ Configuration

USBackup uses a decentralized configuration model. 

1. **Local Config**: Stored in `~/.config/usbackup/backup_config.toml`, it contains the list of approved UUIDs and your machine-specific **secret key**.
2. **Logs**: Detailed logs are kept in `~/.local/share/usbackup/logs/`.
3. **`.usbackup.toml`** (Device): Stored on the root of your USB key. It contains the backup rules and the HMAC signature.

### Example `.usbackup.toml`:

```toml
name = "My SanDisk Key"
vendor_id = 1921
product_id = 21889
uuid = "1234-ABCD"
signature = "a1b2c3d4..."
action = "Whitelist"

[[backup_rules]]
source_path = "/home/user/Documents"
destination_path = "backups/docs"
exclude = [".tmp", "cache/"]
incremental = true           # Enable snapshots
unmount_after = true        # Auto unmount
post_backup_script = "notify-send 'Backup Done!'"
```

### Available Actions:
- `Whitelist`: Automatically triggers backups.
- `IgnoreForever`: Stops asking and ignores the device.
- `AskEachTime`: Prompts for action every time the device is plugged in.

## 🚀 Roadmap

- [x] Decentralized TOML configuration.
- [x] HMAC Signature for configuration security.
- [x] Automatic partition mounting.
- [x] Standard XDG paths for config and logs.
- [x] Incremental snapshots (versioning).
- [x] Post-backup actions (script/unmount).
- [x] System notifications.
- [ ] Multi-target backup (Remote SSH/Cloud).

## ⚖️ License

MIT
