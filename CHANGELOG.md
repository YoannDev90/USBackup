# Changelog

All notable changes to this project are documented in this file.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Pure-Rust sync engine** — an in-process, portable alternative to the
  external `rsync` binary, gated behind the `sync-engine` cargo feature.
  Off by default: `rsync` remains the shipped backend until the engine is
  validated end-to-end on Linux, macOS and Windows.
  - Metadata-only change detection (mtime + size, 2 s FAT tolerance) with
    per-rule excludes, bundled defaults and `.gitignore` — honored on
    incremental snapshots too.
  - Atomic copy (temp file + rename) preserving mtime and permissions;
    symlinks are kept as-is, never dereferenced.
  - Bounded parallel execution with separate metadata and bulk-I/O pools.
  - Incremental hard-link snapshots with retention, or a plain mirror.
  - Optional delta transfer (Rabin-Karp rolling hash) for large files,
    off by default.
  - No `unsafe`, no C toolchain — cross-compiles to Linux, macOS, Windows.
- **Criterion benchmark suite** (`benches/sync_engine.rs`) covering scan
  throughput, cold/noop sync at 10 k and 100 k files, snapshot linking, and
  the pool-size sweeps behind the default tuning (metadata 8, bulk 2,
  1 MiB bulk threshold).
- **Windows support** — USBackup now runs natively on Windows (x64 and
  arm64 release `.exe`):
  - Device discovery through WMI (`Win32_DiskDrive` joined to
    `Win32_LogicalDisk` by volume serial), keeping VID/PID + serial
    identification; Windows auto-mounts volumes itself, so no `udisksctl`
    equivalent is involved.
  - Background agent via a Task Scheduler logon task
    (`usbackup --install-service` + `usbackup.xml`) running in the
    interactive session, so toast notifications and `%APPDATA%` keep
    working.
  - **`install.ps1`** PowerShell installer mirroring `install.sh`
    (SHA256 verification, `%LOCALAPPDATA%` install, optional scheduled
    task).
  - Incremental snapshots are disabled there: the rsync ports cannot
    create the hard links `--link-dest` needs, so `incremental` is forced
    to `false` at backup time with a logged warning and backups run as
    plain mirrors (README "Windows notes").
  - `$PATH` lookup understands `PATHEXT`, so a bare `rsync` resolves to
    `rsync.exe`.
- **Platform device layer** — `udev`/`udisksctl` extracted from
  `udev_utils.rs` into `src/device/` behind `#[cfg(target_os = ...)]`,
  with a shared public API (`find_usb_partitions`,
  `find_partition_by_uuid`, `mount_partition`, `unmount_by_mount_point`,
  `is_removable_mount`, `to_devnode`) and a matching `src/service/`
  module (systemd on Linux, Task Scheduler on Windows, launchd on macOS).
- **macOS support** — USBackup now runs natively on macOS (universal
  binary for Apple Silicon and Intel):
  - Device discovery through `diskutil list -plist external physical`
    (with a per-disk fallback for volumes without a UUID) and
    `system_profiler SPUSBDataType -json` for the USB vendor/product ids;
    volumes auto-mount under `/Volumes`, so no udisks equivalent is
    involved.
  - Background agent via a launchd LaunchAgent
    (`usbackup --install-service` + `usbackup.plist`) loaded into the
    user's `gui` domain with `launchctl bootstrap`; logs at
    `~/Library/Logs/usbackup.log`.
  - **`install.sh` supports Darwin** — fetches the universal
    `usbackup-macos-universal` asset and falls back to `shasum -a 256`
    for the checksum (macOS ships no `sha256sum`).
  - **`usbackup daemon`** is now an accepted explicit argument: the
    service templates pass it (the Windows task previously exited with
    code 2 at every logon because `main.rs` rejected it).

### Changed

- **Dependency checks are platform-aware** — `rsync` + `udisksctl` on
  Linux, `rsync` only on Windows (the OS auto-mounts), `rsync` +
  `diskutil` on macOS.
- **Documentation split per platform** — OS-specific details
  (dependencies, service setup, behaviour, file locations) moved from
  the README to `docs/linux.md`, `docs/windows.md` and `docs/macos.md`;
  the README stays platform-neutral and links to the guides.

### CI

- **Windows legs** — `rust.yml` runs build, tests and a `--version` smoke
  test on `windows-latest` next to Ubuntu; `release.yml` builds
  `usbackup-windows-x64.exe` and `usbackup-windows-arm64.exe` and attaches
  them to the release with SHA256SUMS.
- **macOS legs** — `rust.yml` also runs on `macos-latest` (tests +
  smoke test), every OS leg runs to completion (`fail-fast: false`), and
  Ubuntu cross-checks the macOS target through `scripts/check-macos.sh`
  (stub-libc `cargo check`, no Apple SDK needed); `release.yml` builds
  both Apple targets and merges them with `lipo` into
  `usbackup-macos-universal`.

## [0.1.0] — 2026-10-15

First stable feature release after the v0.0.x experimental series. Focus on
correctness, security hardening, headless operation, and a proper CLI.

### Added

- **CLI surface** — `usbackup` with subcommands and flags:
  - `approve <uuid> [--configure]` — approve a device detected without a TTY
  - `list-devices` — show whitelist / ask-each-time / ignored / pending
  - `--install-service` — explicit systemd user-service install (no longer a
    side effect of every daemon start)
  - `--help`, `--version`
- **Headless-safe UX** — under systemd (no TTY) unknown devices are queued in
  `pending_devices.toml` and a desktop notification points to
  `usbackup approve <uuid>`. Interactive prompts only run on a real TTY.
- **Device actions implemented** — `Whitelist`, `AskEachTime`, and
  `IgnoreForever` are now honoured (previously defined but never read).
- **Verbose logging** — `-v` (debug) / `-vv` (trace) / `-q` (warn), or
  `RUST_LOG` for fine-grained control. Debug output covers the full USB scan,
  mount search, rsync command lines, and config decisions.
- **`curl | bash` installer** (`install.sh`) — detects OS/arch, downloads the
  matching release binary, verifies its SHA256 checksum, installs to
  `~/.local/bin`, and optionally sets up the systemd user service.
- **Unit tests** — HMAC sign/verify round-trip, tamper detection, canonical
  serialization, and destination-path sanitization (12 tests, no hardware
  required).
- **Multi-arch release builds** — amd64 and arm64 binaries with SHA256SUMS
  attached to every release.
- **Package metadata** — `rust-version`, `description`, `license`, `readme`
  in `Cargo.toml`; release builds use LTO and symbol stripping.

### Changed

- **HMAC over a stable canonical form** — signing no longer depends on
  `toml::to_string` output (whose formatting is not a stability guarantee
  across crate versions). *Existing on-key signatures must be regenerated
  via the wizard.*
- **USB identification** — partitions are now matched by the VID/PID of the
  device that triggered the hotplug event *and* the filesystem UUID, so a
  second plugged-in key is never misidentified.
- **Unmount via real block device** — `udisksctl unmount -b` is given the
  resolved `/dev/...` node (not a mount point); failures are logged and
  surfaced as notifications.
- **Post-backup scripts are supervised** — run to completion with exit-code
  checking; failures mark the backup as errored.
- **TTY interactions serialized** — a process-wide tokio Mutex prevents two
  simultaneous wizards from corrupting each other's terminal input.
- **`reqwest` replaced with `ureq`** — much smaller binary; tokio features
  trimmed from `full` to what the code actually uses.
- **`rsync -z` dropped** — compression only pays off over the network.
- **Config written `0600`** — the file contains the HMAC secret key.
- **Logs rotated** at 10 MB per device (single `.1` previous generation).
- **Dependencies checked via `$PATH`**, not the `which` binary (absent on
  minimal systems).
- **All log messages and comments unified to English** (`README_FR.md`
  removed).

### Fixed

- **Corrupt config no longer overwritten** — a parse error previously
  replaced `backup_config.toml` with a fresh secret, losing every approved
  UUID and invalidating every device signature. The file is now kept
  untouched and an in-memory default is used.
- **Nested tokio runtime removed** around the wizard.
- **Wizard mount selection is structured** — no more `split(" on ")` on a
  formatted string (broke if a disk name contained " on ").
- **`pending_devices.toml` serialization** — wrapped in a `PendingFile`
  struct; TOML has no top-level array value.
- **systemd unit hardened** — quoted `ExecStart`, `TimeoutStopSec=60`,
  `NoNewPrivileges=yes`, `SyslogIdentifier`; dropped the unnecessary
  `After=network.target`.

### CI

- Release workflow builds both `linux-amd64` and `linux-arm64`, generates
  SHA256SUMS, and attaches everything via `gh release create`.
- Actions pinned to full commit SHAs; least-privilege `permissions` blocks
  added.

## [0.0.2] — 2026-04-26

- Incremental snapshot support via `rsync --link-dest`.
- Systemd user-service auto-install on first run.
- XDG-compliant config and log paths.
- Documentation overhaul.

## [0.0.1] — 2026-04-25

- Switched device config extension to `.usbackup.toml`.
- Improved UUID retrieval logic for USB partitions.

## [0.0.0] — 2026-04-25

- Initial experimental release.
- Decentralized TOML configuration stored on the USB key.
- HMAC-SHA256 signature verification with a per-machine secret.
- Hotplug detection via `nusb`, auto-mount via `udisksctl`.
- Interactive device whitelisting wizard.
- Post-backup actions (script / unmount).

[Unreleased]: https://github.com/YoannDev90/USBackup/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/YoannDev90/USBackup/compare/v0.0.2...v0.1.0
[0.0.2]: https://github.com/YoannDev90/USBackup/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/YoannDev90/USBackup/compare/v0.0.0...v0.0.1
[0.0.0]: https://github.com/YoannDev90/USBackup/releases/tag/v0.0.0
