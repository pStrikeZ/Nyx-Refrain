#!/usr/bin/env bash
set -euo pipefail

echo "=== Checking toolchain ==="

# Check Rust
if ! command -v rustc >/dev/null 2>&1; then
    echo "ERROR: rustc not found" >&2
    exit 1
fi
echo "rustc: $(rustc --version)"

# Check / install targets
TARGETS=(
    x86_64-pc-windows-gnu
    aarch64-pc-windows-gnullvm
    x86_64-unknown-linux-musl
    aarch64-unknown-linux-musl
    loongarch64-unknown-linux-musl
)

for target in "${TARGETS[@]}"; do
    if rustup target list --installed | grep -q "^${target}$"; then
        echo "Target ${target}: already installed"
    else
        echo "Target ${target}: installing via rustup..."
        rustup target add "${target}"
    fi
done

# Check zig
if command -v zig >/dev/null 2>&1; then
    echo "zig: $(zig version) at $(which zig)"
else
    echo "ERROR: zig not found in PATH" >&2
    exit 1
fi

# Check cargo-zigbuild
if command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "cargo-zigbuild: $(cargo-zigbuild --version) at $(which cargo-zigbuild)"
else
    echo "Installing cargo-zigbuild via cargo install..."
    cargo install cargo-zigbuild
fi

# Check inspection tools
for tool in readelf llvm-readobj llvm-objdump objdump; do
    if command -v "${tool}" >/dev/null 2>&1; then
        echo "${tool}: found at $(which "${tool}")"
    else
        echo "NOTE: ${tool} not found (optional/fallback tool)"
    fi
done

# Check emulation tools (for smoke testing non-native binaries)
for emu in qemu-aarch64 qemu-loongarch64; do
    if command -v "${emu}" >/dev/null 2>&1; then
        echo "${emu}: found at $(which "${emu}")"
    else
        echo "NOTE: ${emu} not found (install qemu-user / qemu-user-static for smoke testing)"
    fi
done

echo "=== Toolchain ready ==="
