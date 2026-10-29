# Linux

Native support: hotplug via `nusb`, partitions resolved through `udev`,
mounting through `udisksctl`. Releases ship amd64 and arm64 binaries.

## Dependencies

| Distro | Command |
| :-- | :-- |
| Debian / Ubuntu | `sudo apt install libudev-dev pkg-config rsync` |
| Fedora / RHEL | `sudo dnf install libudev-devel pkg-config rsync` |

`libudev-dev` and `pkg-config` are build dependencies; `rsync` and
`udisksctl` (part of `udisks2`) are required at runtime.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/YoannDev90/USBackup/master/install.sh | bash
```

The script downloads the latest release binary for your architecture,
verifies its SHA256 checksum, installs it to `~/.local/bin`, and offers
to set up the systemd user service.

<details>
<summary>Installer options</summary>

```bash
# pass flags after `--` when piping:
curl -fsSL ... | bash -s -- --prefix ~/.local/bin   # custom install dir
curl -fsSL ... | bash -s -- --version 0.1.0          # pin a version
curl -fsSL ... | bash -s -- --no-service             # skip the service prompt
curl -fsSL ... | bash -s -- --force                  # overwrite existing binary
```

</details>

## Background service (systemd user)

```bash
usbackup --install-service
systemctl --user start usbackup.service
```

Stop or inspect it with the usual systemd commands:

```bash
systemctl --user stop usbackup.service
systemctl --user status usbackup.service
```

## Behaviour

- **Discovery & mounting** — partitions are scanned from `udev` and matched
  by VID/PID of the triggering device *and* filesystem UUID; the key is
  mounted with `udisksctl` and unmounted through the resolved `/dev/...`
  block device node.
- **Incremental snapshots** — fully supported (`rsync --link-dest`
  hard-link versioning with a `latest` symlink).
- **Headless** — running without a TTY (e.g. under systemd) queues unknown
  devices as *pending* and raises a desktop notification pointing to
  `usbackup approve <uuid>`.

## File locations

| What | Path |
| :-- | :-- |
| Local config (secret key + whitelists) | `~/.config/usbackup/backup_config.toml` (`0600`) |
| Pending device queue | `~/.config/usbackup/pending_devices.toml` |
| Per-device rsync logs | `~/.local/share/usbackup/logs/<name>.log` (rotated at 10 MB) |
| systemd user service | `~/.config/systemd/user/usbackup.service` |
