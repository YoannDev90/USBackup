# USBackup 🚀

Automated USB backup agent written in Rust. This program runs 24/7 and instantly detects USB device connections to trigger predefined actions.

[Version Française ici (French Version)](README_FR.md)

## ✨ Features

- **Real-time Monitoring**: Detects connect/disconnect events without polling (using native APIs via `nusb`).
- **Interactive Whitelisting**: When a new device is detected, the app asks whether to remember it, ignore it, or ask again later.
- **Flexible Configuration**: Manage specific backup rules (sources, destinations, exclusions) for each device.
- **Colored Logs**: Clear activity tracking directly in your terminal.

## 🛠️ Installation

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

- [ ] Implement automatic partition mounting (Linux/Udisksctl).
- [ ] Add synchronization logic via `rsync`.
- [ ] System notifications upon backup completion.

## ⚖️ License

MIT
