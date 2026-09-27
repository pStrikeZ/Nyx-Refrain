#!/usr/bin/env bash
# Package the requested dist/Linux architectures with a pinned, local nfpm.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ $# -eq 0 ]]; then
    echo "Usage: $0 x86_64 [aarch64 loongarch64]" >&2
    exit 1
fi
if [[ "$(uname -s)/$(uname -m)" != "Linux/x86_64" ]]; then
    echo "ERROR: Package builds require a Linux x86_64 host for the pinned nfpm binary." >&2
    exit 1
fi

# v2.44.2 introduced loong64 -> loongarch64 for RPM; deb/Arch keep loong64.
# https://github.com/goreleaser/nfpm/commit/89dc45ce278f1e8bc68b3c5bc91d3a283b7c8139
NFPM_VERSION=2.47.0
# https://github.com/goreleaser/nfpm/releases/download/v2.47.0/checksums.txt
NFPM_SHA256=0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783
NFPM_DIR="${REPO_ROOT}/target/tools/nfpm-${NFPM_VERSION}"
NFPM_ARCHIVE="nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz"

PACKAGE_WORK_DIR="$(mktemp -d)"
export PACKAGE_WORK_DIR
trap 'rm -rf "${PACKAGE_WORK_DIR}"' EXIT

if [[ ! -f "${NFPM_DIR}/${NFPM_ARCHIVE}" ]]; then
    curl --fail --location --retry 3 \
        "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/${NFPM_ARCHIVE}" \
        --output "${PACKAGE_WORK_DIR}/${NFPM_ARCHIVE}"
    echo "${NFPM_SHA256}  ${PACKAGE_WORK_DIR}/${NFPM_ARCHIVE}" | sha256sum --check
    mkdir -p "${NFPM_DIR}"
    mv "${PACKAGE_WORK_DIR}/${NFPM_ARCHIVE}" "${NFPM_DIR}/${NFPM_ARCHIVE}"
fi
echo "${NFPM_SHA256}  ${NFPM_DIR}/${NFPM_ARCHIVE}" | sha256sum --check
# Re-extract only the executable from the verified cache; no system installation.
tar -xzf "${NFPM_DIR}/${NFPM_ARCHIVE}" -C "${NFPM_DIR}" nfpm

PACKAGE_VERSION="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"
PACKAGE_MAINTAINER="$(git config user.name) <$(git config user.email)>"
export PACKAGE_VERSION PACKAGE_MAINTAINER
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"

{
    printf 'Nyx Refrain is licensed under the MIT License.\n\n'
    cat LICENSE NOTICE LICENSE-fluentui-emoji
} > "${PACKAGE_WORK_DIR}/copyright"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate packaging/linux/nyx-refrain.desktop
fi

mkdir -p dist/packages
for arch in "$@"; do
    case "${arch}" in
        x86_64) PACKAGE_ARCH=amd64; archlinux_arch=x86_64 ;;
        aarch64) PACKAGE_ARCH=arm64; archlinux_arch=aarch64 ;;
        loongarch64) PACKAGE_ARCH=loong64; archlinux_arch=loong64 ;;
        *) echo "ERROR: Unsupported Linux architecture: ${arch}" >&2; exit 1 ;;
    esac
    PACKAGE_BIN_DIR="dist/linux-${arch}"
    export PACKAGE_ARCH PACKAGE_BIN_DIR
    for format in deb rpm archlinux; do
        "${NFPM_DIR}/nfpm" package --config packaging/linux/nfpm.yaml \
            --packager "${format}" --target dist/packages/
    done
    python3 packaging/linux/finalize-arch.py \
        "dist/packages/nyx-refrain-${PACKAGE_VERSION}-1-${archlinux_arch}.pkg.tar.zst"
done
