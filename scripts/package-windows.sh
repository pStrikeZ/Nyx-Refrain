#!/usr/bin/env bash
# Builds Windows installers (NSIS) from dist/win32-amd64 and dist/win32-aarch64:
#   dist/packages/nyx-refrain-<version>-windows-{x86_64,arm64}-setup.exe
# Requires makensis with amd64 stubs (installer.nsi uses Target amd64-unicode): Debian/Ubuntu
# `apt install nsis`. Arch's AUR nsis only builds x86 stubs; use a Debian container instead:
#   docker run --rm -v "$PWD":/src -w /src debian:stable sh -c \
#     'apt-get update && apt-get install -y nsis && setpriv --reuid=UID --regid=GID --clear-groups ./scripts/package-windows.sh'
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v makensis >/dev/null 2>&1; then
    echo "ERROR: makensis not found (install the 'nsis' package)" >&2
    exit 1
fi

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
mkdir -p dist/packages

built=0
for pair in "win32-amd64:x86_64" "win32-aarch64:arm64"; do
    dist_sub="${pair%%:*}"
    arch="${pair##*:}"
    [[ -f "dist/${dist_sub}/nyx-refrain.exe" ]] || continue
    out="dist/packages/nyx-refrain-${VERSION}-windows-${arch}-setup.exe"
    echo "--> Windows installer (${arch}): ${out}"
    makensis -V2 -NOCD -INPUTCHARSET UTF8 \
        -DVERSION="${VERSION}" -DARCH="${arch}" -DDIST_DIR="dist/${dist_sub}" \
        -DOUT_FILE="${out}" packaging/windows/installer.nsi
    built=$((built + 1))
done

if [[ ${built} -eq 0 ]]; then
    echo "No Windows binaries in dist/win32-amd64 or dist/win32-aarch64; nothing to package." >&2
    exit 1
fi
