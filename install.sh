#!/usr/bin/env bash
# USBackup installer — downloads the latest release binary from GitHub.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/YoannDev90/USBackup/master/install.sh | bash
#
#   # or with options:
#   curl -fsSL ... | bash -s -- --prefix ~/.local --no-service
#
# Options (pass after -- when piping):
#   --prefix DIR     Install directory (default: ~/.local/bin)
#   --version X.Y.Z  Install a specific version instead of latest
#   --no-service     Skip the background agent service install prompt
#   --force          Overwrite an existing binary without asking

set -euo pipefail

REPO="YoannDev90/USBackup"
BIN_NAME="USBackup"
INSTALL_PREFIX="${HOME}/.local/bin"
REQUESTED_VERSION=""
INSTALL_SERVICE="ask"   # ask | yes | no
FORCE=0

# ---------------------------------------------------------------- argument parsing
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)     INSTALL_PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        --version)    REQUESTED_VERSION="${2:?--version needs a value}"; shift 2 ;;
        --no-service) INSTALL_SERVICE="no"; shift ;;
        --force)      FORCE=1; shift ;;
        -h|--help)
            sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            exit 2
            ;;
    esac
done

# ---------------------------------------------------------------- helpers
info()  { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn()  { printf '  \033[33m!\033[0m %s\n' "$*" >&2; }
die()   { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed."
}

# ---------------------------------------------------------------- preflight
echo "USBackup installer"
echo

need curl
need tar
# macOS ships shasum, not sha256sum.
if command -v sha256sum >/dev/null 2>&1; then
    SHA256_CMD=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
    SHA256_CMD=(shasum -a 256)
else
    die "'sha256sum' or 'shasum' is required but not installed."
fi

case "$(uname -s)" in
    Linux)  OS="linux" ;;
    Darwin) OS="macos" ;;
    *)      die "Unsupported OS: $(uname -s). USBackup supports Linux and macOS." ;;
esac

if [[ "$OS" == "macos" ]]; then
    # Releases ship a single universal binary for both Mac architectures.
    ASSET="usbackup-macos-universal"
else
    case "$(uname -m)" in
        x86_64|amd64)  ARCH="amd64" ;;
        aarch64|arm64) ARCH="arm64" ;;
        *)             die "Unsupported architecture: $(uname -m)" ;;
    esac
    ASSET="usbackup-${OS}-${ARCH}"
fi
API="https://api.github.com/repos/${REPO}/releases"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

# ---------------------------------------------------------------- resolve version
if [[ -n "$REQUESTED_VERSION" ]]; then
    VERSION="$REQUESTED_VERSION"
    RELEASE_URL="${API}/tags/v${VERSION}"
else
    RELEASE_URL="${API}/latest"
fi

echo "→ Resolving release from ${RELEASE_URL} …"
RELEASE_JSON="$(curl -fsSL \
    -H 'Accept: application/vnd.github+json' \
    -H 'User-Agent: usbackup-installer' \
    "$RELEASE_URL")" || die "Could not fetch release info. Does this version exist?"

VERSION="$(printf '%s' "$RELEASE_JSON" \
    | grep -o '"tag_name": *"[^"]*"' | head -1 \
    | sed 's/.*: *"//; s/"$//; s/^v//')"

[[ -n "$VERSION" ]] || die "Could not parse release tag from GitHub response."

# Find the browser_download_url for our asset
DOWNLOAD_URL="$(printf '%s' "$RELEASE_JSON" \
    | grep -o '"browser_download_url": *"[^"]*'"${ASSET}"'"' \
    | head -1 \
    | sed 's/.*: *"//; s/"$//')"

[[ -n "$DOWNLOAD_URL" ]] || die "No release asset found for '${ASSET}' (version ${VERSION})."

info "Version ${VERSION} — ${ASSET}"

# ---------------------------------------------------------------- download
echo "→ Downloading ${ASSET} …"
curl -fsSL -o "${TMP_DIR}/${ASSET}" "$DOWNLOAD_URL"
info "Binary downloaded"

# ---------------------------------------------------------------- checksum
CHECKSUMS_URL="${DOWNLOAD_URL%/*}/SHA256SUMS"
echo "→ Verifying SHA256 checksum …"
if curl -fsSL -o "${TMP_DIR}/SHA256SUMS" "$CHECKSUMS_URL" 2>/dev/null; then
    EXPECTED="$(grep -E "[ /]${ASSET}\$" "${TMP_DIR}/SHA256SUMS" | awk '{print $1}' | head -1)"
    if [[ -z "$EXPECTED" ]]; then
        warn "No checksum entry for ${ASSET} in SHA256SUMS; skipping verification."
    else
        ACTUAL="$("${SHA256_CMD[@]}" "${TMP_DIR}/${ASSET}" | awk '{print $1}')"
        [[ "$ACTUAL" == "$EXPECTED" ]] || die "Checksum verification FAILED — refusing to install."
        info "Checksum OK"
    fi
else
    warn "SHA256SUMS not found in this release; skipping verification."
fi

chmod +x "${TMP_DIR}/${ASSET}"

# ---------------------------------------------------------------- install binary
mkdir -p "$INSTALL_PREFIX"
DEST="${INSTALL_PREFIX}/${BIN_NAME}"

if [[ -e "$DEST" && "$FORCE" -ne 1 ]]; then
    if [[ -t 0 ]]; then
        read -r -p "${DEST} already exists. Overwrite? [y/N] " ans
        [[ "$ans" =~ ^[Yy]$ ]] || die "Aborted."
    else
        die "${DEST} exists. Re-run with --force to overwrite."
    fi
fi

install -m 0755 "${TMP_DIR}/${ASSET}" "$DEST"
info "Installed to ${DEST}"

# PATH hint
case ":${PATH}:" in
    *":${INSTALL_PREFIX}:"*) ;;
    *)
        warn "${INSTALL_PREFIX} is not in your PATH. Add this to your shell profile:"
        echo "      export PATH=\"${INSTALL_PREFIX}:\$PATH\""
        ;;
esac

# ---------------------------------------------------------------- background agent service
if [[ "$INSTALL_SERVICE" == "ask" ]]; then
    if [[ -t 0 ]] && { command -v systemctl >/dev/null 2>&1 || command -v launchctl >/dev/null 2>&1; }; then
        read -r -p "Install and enable the background agent service? [Y/n] " ans
        INSTALL_SERVICE="${ans:-yes}"
        [[ "$ans" =~ ^[Nn]$ ]] && INSTALL_SERVICE="no" || INSTALL_SERVICE="yes"
    else
        INSTALL_SERVICE="no"
        echo
        info "Tip: run '${DEST} --install-service' to enable the background agent."
    fi
fi

if [[ "$INSTALL_SERVICE" == "yes" ]]; then
    echo "→ Installing the background agent service …"
    if "$DEST" --install-service; then
        info "Service installed."
    else
        warn "Service installation failed; you can retry with: ${DEST} --install-service"
    fi
fi

echo
info "Done! Run '${BIN_NAME} --help' to get started."
