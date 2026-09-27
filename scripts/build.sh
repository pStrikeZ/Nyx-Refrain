#!/usr/bin/env bash
# Consolidated multi-target build script for Nyx Refrain.
#
# Builds CLI (nyxr) and GUI (nyx-refrain) for requested targets in a single
# cargo zigbuild invocation so dependencies compile once and targets link in parallel.
#
# Supported target aliases:
#   win-x64         -> x86_64-pc-windows-gnu
#   win-arm64       -> aarch64-pc-windows-gnullvm
#   linux-x64       -> x86_64-unknown-linux-musl
#   linux-arm64     -> aarch64-unknown-linux-musl
#   linux-loong64   -> loongarch64-unknown-linux-musl
#
# Usage:
#   ./scripts/build.sh                                     # build all 5 targets (default)
#   TARGETS="win-x64 linux-x64" ./scripts/build.sh         # build subset using aliases
#   TARGETS="x86_64-pc-windows-gnu" ./scripts/build.sh     # build subset using Rust triples
#   ./scripts/build.sh win-x64 linux-x64                   # arguments also accepted
#   NO_PACKAGES=1 ./scripts/build.sh                      # skip Linux packages and Windows installers
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

# Available targets and aliases
declare -A ALIAS_MAP=(
    ["win-x64"]="x86_64-pc-windows-gnu"
    ["win-arm64"]="aarch64-pc-windows-gnullvm"
    ["linux-x64"]="x86_64-unknown-linux-musl"
    ["linux-arm64"]="aarch64-unknown-linux-musl"
    ["linux-loong64"]="loongarch64-unknown-linux-musl"
)

declare -A TARGET_OS=(
    ["x86_64-pc-windows-gnu"]="windows"
    ["aarch64-pc-windows-gnullvm"]="windows"
    ["x86_64-unknown-linux-musl"]="linux"
    ["aarch64-unknown-linux-musl"]="linux"
    ["loongarch64-unknown-linux-musl"]="linux"
)

declare -A TARGET_DIST_DIR=(
    ["x86_64-pc-windows-gnu"]="win32-amd64"
    ["aarch64-pc-windows-gnullvm"]="win32-aarch64"
    ["x86_64-unknown-linux-musl"]="linux-x86_64"
    ["aarch64-unknown-linux-musl"]="linux-aarch64"
    ["loongarch64-unknown-linux-musl"]="linux-loongarch64"
)

DEFAULT_TARGETS="win-x64 win-arm64 linux-x64 linux-arm64 linux-loong64"
REQUESTED_INPUT="${TARGETS:-${*:-${DEFAULT_TARGETS}}}"

# Resolve aliases to standard rust triples and deduplicate while preserving order
RESOLVED_TARGETS=()
declare -A SEEN_TARGETS=()

for item in ${REQUESTED_INPUT}; do
    triple="${ALIAS_MAP[$item]:-$item}"
    if [[ -z "${TARGET_OS[$triple]:-}" ]]; then
        echo "ERROR: Unknown target '${item}' (resolved as '${triple}')." >&2
        echo "Supported targets / aliases: ${!ALIAS_MAP[*]} or ${!TARGET_OS[*]}" >&2
        exit 1
    fi
    if [[ -z "${SEEN_TARGETS[$triple]:-}" ]]; then
        SEEN_TARGETS["$triple"]=1
        RESOLVED_TARGETS+=("$triple")
    fi
done

echo "=== Nyx Refrain Multi-Target Build ==="
echo "Targets: ${RESOLVED_TARGETS[*]}"

# Check required build tools
for cmd in zig cargo-zigbuild; do
    if ! command -v "${cmd}" >/dev/null 2>&1; then
        echo "ERROR: Required tool '${cmd}' not found in PATH." >&2
        exit 1
    fi
done

# Ensure rustup targets are installed
if command -v rustup >/dev/null 2>&1; then
    installed_targets="$(rustup target list --installed)"
    missing_targets=()
    for t in "${RESOLVED_TARGETS[@]}"; do
        if ! grep -q "^${t}$" <<< "${installed_targets}"; then
            missing_targets+=("${t}")
        fi
    done
    if [[ ${#missing_targets[@]} -gt 0 ]]; then
        echo "--> Installing missing rustup targets: ${missing_targets[*]}"
        rustup target add "${missing_targets[@]}"
    fi
fi

# Configure zig caches under target/
TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
export ZIG_GLOBAL_CACHE_DIR="${TARGET_DIR}/zig-global-cache"
export ZIG_LOCAL_CACHE_DIR="${TARGET_DIR}/zig-local-cache"
mkdir -p "${ZIG_GLOBAL_CACHE_DIR}" "${ZIG_LOCAL_CACHE_DIR}"

# Remap path flags for clean, reproducible paths
HOME_DIR="${HOME:-${USERPROFILE:-}}"
CARGO_HOME_DIR="${CARGO_HOME:-${HOME_DIR:+$HOME_DIR/.cargo}}"

REMAP_FLAGS=()
if [[ -n "${HOME_DIR}" ]]; then
    REMAP_FLAGS+=("--remap-path-prefix=${HOME_DIR}=~")
fi
if [[ -n "${CARGO_HOME_DIR}" ]]; then
    REMAP_FLAGS+=("--remap-path-prefix=${CARGO_HOME_DIR}/registry/src=cargo-registry")
    REMAP_FLAGS+=("--remap-path-prefix=${CARGO_HOME_DIR}/git/checkouts=cargo-git")
fi
REMAP_FLAGS+=("--remap-path-prefix=${REPO_ROOT}=.")

if [[ -n "${CARGO_ENCODED_RUSTFLAGS+x}" ]]; then
    remap_encoded="$(printf "%s\x1f" "${REMAP_FLAGS[@]}")"
    remap_encoded="${remap_encoded%\x1f}"
    if [[ -n "${CARGO_ENCODED_RUSTFLAGS}" ]]; then
        export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}"$'\x1f'"${remap_encoded}"
    else
        export CARGO_ENCODED_RUSTFLAGS="${remap_encoded}"
    fi
else
    remap_str="${REMAP_FLAGS[*]}"
    if [[ -n "${RUSTFLAGS:-}" ]]; then
        export RUSTFLAGS="${RUSTFLAGS} ${remap_str}"
    else
        export RUSTFLAGS="${remap_str}"
    fi
fi

# Single cargo zigbuild call for all targets and binaries
CARGO_TARGET_ARGS=()
LINUX_PACKAGE_ARCHES=()
has_windows=0
has_linux=0

for t in "${RESOLVED_TARGETS[@]}"; do
    CARGO_TARGET_ARGS+=("--target" "${t}")
    if [[ "${TARGET_OS[$t]}" == "windows" ]]; then
        has_windows=1
    elif [[ "${TARGET_OS[$t]}" == "linux" ]]; then
        has_linux=1
        LINUX_PACKAGE_ARCHES+=("${TARGET_DIST_DIR[$t]#linux-}")
    fi
done

echo "--> Building all binaries in a single invocation..."
cargo zigbuild --locked --release "${CARGO_TARGET_ARGS[@]}" --bin nyx-refrain --bin nyxr

# Packaging: clean only the subdirectories being rebuilt
mkdir -p dist

for t in "${RESOLVED_TARGETS[@]}"; do
    dist_sub="${TARGET_DIST_DIR[$t]}"
    out_dir="dist/${dist_sub}"
    rm -rf "${out_dir}"
    mkdir -p "${out_dir}"

    if [[ "${TARGET_OS[$t]}" == "windows" ]]; then
        cp -f "target/${t}/release/nyxr.exe" "${out_dir}/nyxr.exe"
        cp -f "target/${t}/release/nyx-refrain.exe" "${out_dir}/nyx-refrain.exe"
    else
        cp -f "target/${t}/release/nyxr" "${out_dir}/nyxr"
        cp -f "target/${t}/release/nyx-refrain" "${out_dir}/nyx-refrain"
    fi
    cp -f LICENSE NOTICE LICENSE-fluentui-emoji "${out_dir}/"
done

if [[ "${has_windows}" -eq 1 && -f docs/windows-quickstart.md ]]; then
    cp -f docs/windows-quickstart.md dist/windows-quickstart.md
fi

if [[ "${has_linux}" -eq 1 && -f docs/linux-quickstart.md ]]; then
    cp -f docs/linux-quickstart.md dist/linux-quickstart.md
fi

# Remove obsolete separate linux checksum file if present
rm -f dist/SHA256SUMS-linux

# Verification inside the script
echo "=== Verifying Artifacts ==="

# 1. Linux verification: fully static (no INTERP, no NEEDED)
for t in "${RESOLVED_TARGETS[@]}"; do
    [[ "${TARGET_OS[$t]}" == "linux" ]] || continue
    dist_sub="${TARGET_DIST_DIR[$t]}"
    for bin in nyxr nyx-refrain; do
        bin_path="dist/${dist_sub}/${bin}"
        echo "Checking ${bin_path} (Linux static)..."
        if [[ ! -f "${bin_path}" ]]; then
            echo "ERROR: Expected artifact ${bin_path} does not exist!" >&2
            exit 1
        fi

        if readelf -l "${bin_path}" 2>/dev/null | grep -q "INTERP"; then
            echo "ERROR: ${bin_path} is not fully static! Found INTERP program header." >&2
            readelf -l "${bin_path}" | grep -B 1 -A 2 "INTERP" >&2
            exit 1
        fi

        if readelf -d "${bin_path}" 2>/dev/null | grep -q "NEEDED"; then
            echo "ERROR: ${bin_path} is not fully static! Found NEEDED dynamic entries:" >&2
            readelf -d "${bin_path}" | grep "NEEDED" >&2
            exit 1
        fi
        echo "  [OK] Fully static (no INTERP, no NEEDED)."
    done
done

# 2. Windows verification: system DLL imports only
for t in "${RESOLVED_TARGETS[@]}"; do
    [[ "${TARGET_OS[$t]}" == "windows" ]] || continue
    dist_sub="${TARGET_DIST_DIR[$t]}"
    for bin in nyxr.exe nyx-refrain.exe; do
        bin_path="dist/${dist_sub}/${bin}"
        echo "Checking ${bin_path} (Windows system DLLs)..."
        if [[ ! -f "${bin_path}" ]]; then
            echo "ERROR: Expected artifact ${bin_path} does not exist!" >&2
            exit 1
        fi

        if command -v llvm-readobj >/dev/null 2>&1; then
            dlls="$(llvm-readobj --coff-imports "${bin_path}" | grep -E '^\s*Name:\s*\S+\.dll' | awk '{print $2}' | sort -u)"
        elif command -v objdump >/dev/null 2>&1; then
            dlls="$(objdump -p "${bin_path}" | grep -i "DLL Name:" | awk '{print $3}' | sort -u)"
        else
            echo "ERROR: Neither llvm-readobj nor objdump is available to inspect ${bin_path}!" >&2
            exit 1
        fi

        if [[ -z "${dlls}" ]]; then
            echo "ERROR: No imported DLLs found for ${bin_path}!" >&2
            exit 1
        fi

        forbidden_patterns='^(libgcc_s|libunwind|libwinpthread|libc\+\+|libstdc\+\+)'
        allowed_pattern='^(kernel32|advapi32|ole32|oleaut32|ntdll|userenv|ws2_32|shell32|user32|bcrypt|bcryptprimitives|iphlpapi|mmdevapi|propsys|avrt|combase|comctl32|dwmapi|dxgi|d3d12|d3dcompiler_47|gdi32|imm32|opengl32|rpcrt4|setupapi|uxtheme|shcore|winmm|version|dbghelp|crypt32|secur32|ncrypt|msvcrt|ucrtbase|api-ms-win-.*)\.dll$'

        failed=0
        while IFS= read -r dll; do
            [[ -z "${dll}" ]] && continue
            lower_dll="$(echo "${dll}" | tr '[:upper:]' '[:lower:]')"
            if echo "${lower_dll}" | grep -qE "${forbidden_patterns}"; then
                echo "ERROR: Forbidden runtime DLL imported: ${dll}" >&2
                failed=1
            fi
            if ! echo "${lower_dll}" | grep -qE "${allowed_pattern}"; then
                echo "ERROR: Non-system DLL imported: ${dll}" >&2
                failed=1
            fi
        done <<< "${dlls}"

        if [[ "${failed}" -ne 0 ]]; then
            echo "ERROR: DLL verification failed for ${bin_path}!" >&2
            exit 1
        fi
        echo "  [OK] System DLLs only."
    done
done

# Package only Linux targets selected for this build.
if [[ ${#LINUX_PACKAGE_ARCHES[@]} -gt 0 && "${NO_PACKAGES:-0}" != 1 ]]; then
    "${SCRIPT_DIR}/package-linux.sh" "${LINUX_PACKAGE_ARCHES[@]}"
fi

# Windows installers (NSIS). Skipped with a warning when makensis is missing so a plain
# build still works; stale installers from earlier builds are removed either way.
if [[ "${has_windows}" -eq 1 && "${NO_PACKAGES:-0}" != 1 ]]; then
    rm -f dist/packages/*-windows-*-setup.exe
    if command -v makensis >/dev/null 2>&1; then
        "${SCRIPT_DIR}/package-windows.sh"
    else
        echo "WARNING: makensis not found; skipping Windows installers (Debian/Ubuntu: apt install nsis; see scripts/package-windows.sh)" >&2
    fi
fi

# Generate SHA256SUMS covering all built artifacts present in dist/ subdirectories
echo "--> Generating dist/SHA256SUMS covering all built artifacts..."
(
    cd dist
    sha_files=()
    for d in win32-amd64 win32-aarch64 linux-x86_64 linux-aarch64 linux-loongarch64 packages; do
        if [[ -d "$d" ]]; then
            for f in "$d"/*; do
                [[ -f "$f" ]] && sha_files+=("$f")
            done
        fi
    done
    if [[ ${#sha_files[@]} -gt 0 ]]; then
        sha256sum "${sha_files[@]}" > SHA256SUMS
    fi
)

echo "=== Build complete ==="
cat dist/SHA256SUMS
