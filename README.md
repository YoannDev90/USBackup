# USBackup 🚀

Automated USB backup agent written in Rust. This program runs 24/7 and instantly detects USB device connections to trigger predefined actions.

[Version Française ici (French Version)](README_FR.md)

## ✨ Features

- **Real-time Monitoring**: Detects connect/disconnect events without polling (using native APIs via `nusb`).
- **Modern TUI**: Beautiful Terminal User Interface built with `ratatui` for real-time activity tracking.
- **Smart Auto-mount**: Automatically finds and mounts USB partitions using `udev` and `udisksctl` (cross-platform logic with Linux special handling).
- **Interactive Whitelisting**: When a new device is detected, the app asks whether to remember it, ignore it, or ask again later.
- **Background Agent**: Multi-threaded architecture to keep the UI responsive during backups.
- **Flexible Configuration**: Manage specific backup rules (sources, destinations, exclusions) for each device.
- **System Notifications**: Uses native desktop notifications to keep you informed.

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

The `backup_config.json` file manages your known devices. Typical structure:

```json
{
  "devices": {
    "0781:5581": {
      "name": "My SanDisk Key",
      "vendor_id": 1921,
      "product_id": 21889,
      "action": "Whitelist",
      "backup_rules": [
        {
          "source_path": "/path/to/usb/data",
          "destination_path": "/home/user/backups/sandisk/",
          "exclude": [".tmp", "cache/"]
        }
      ]
    }
  }
}
```

### Available Actions:
- `Whitelist`: Automatically triggers backups.
- `IgnoreForever`: Stops asking and ignores the device.
- `AskEachTime`: Prompts for action every time the device is plugged in.

## 🚀 Roadmap

- [x] Implement automatic partition mounting (udev/udisksctl).
- [x] Modern TUI with real-time logs.
- [ ] Add synchronization logic via `rsync` or native Rust copy.
- [x] System notifications upon backup completion.

## ⚖️ License

MIT
