#!/usr/bin/env bash
# Type-check the macOS build from a non-mac host.
#
# This is the macOS compile gate when no macOS CI runner is available:
# `cargo check` never links, mac-notification-sys' build script is skipped
# via DOCS_RS=1 (its documented docs.rs escape hatch), and the C
# dependencies (ring, via ureq) are compiled by clang against minimal libc
# stubs with an explicit arm64-apple-macos target.
#
# Usage: scripts/check-macos.sh [cargo check args]   (default: --tests)
# Requires: rustup, clang. Runs on Linux and macOS alike.
set -euo pipefail

cd "$(dirname "$0")/.."

command -v clang >/dev/null || { echo "error: clang not found" >&2; exit 1; }
rustup target list --installed 2>/dev/null | grep -q '^aarch64-apple-darwin$' \
    || rustup target add aarch64-apple-darwin

args=("$@")
if [ ${#args[@]} -eq 0 ]; then
    args=(--tests)
fi

STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT

cat > "$STUB/assert.h" <<'EOF'
#ifndef STUB_ASSERT_H
#define STUB_ASSERT_H
#define assert(x) ((void)0)
#endif
EOF

cat > "$STUB/stdlib.h" <<'EOF'
#ifndef STUB_STDLIB_H
#define STUB_STDLIB_H
#include <stddef.h>
void *malloc(size_t size);
void free(void *ptr);
void *realloc(void *ptr, size_t size);
void *calloc(size_t nmemb, size_t size);
_Noreturn void abort(void);
_Noreturn void exit(int status);
#endif
EOF

cat > "$STUB/string.h" <<'EOF'
#ifndef STUB_STRING_H
#define STUB_STRING_H
#include <stddef.h>
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *s, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
size_t strlen(const char *s);
char *strcpy(char *dst, const char *src);
#endif
EOF

cat > "$STUB/TargetConditionals.h" <<'EOF'
#ifndef STUB_TARGETCONDITIONALS_H
#define STUB_TARGETCONDITIONALS_H
#define TARGET_OS_MAC 1
#define TARGET_OS_OSX 1
#define TARGET_OS_IPHONE 0
#define TARGET_OS_IOS 0
#define TARGET_OS_EMBEDDED 0
#define TARGET_IPHONE_SIMULATOR 0
#define TARGET_OS_WIN32 0
#define TARGET_CPU_X86_64 0
#define TARGET_CPU_ARM64 1
#define TARGET_RT_64_BIT 1
#define TARGET_RT_LITTLE_ENDIAN 1
#endif
EOF

DOCS_RS=1 \
CC_aarch64_apple_darwin=clang \
CFLAGS_aarch64_apple_darwin="--target=arm64-apple-macos11 -nostdinc -isystem $STUB -isystem $(clang -print-resource-dir)/include" \
cargo check --target aarch64-apple-darwin "${args[@]}"
