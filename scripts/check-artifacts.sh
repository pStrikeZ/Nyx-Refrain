#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

OBJDUMP="${OBJDUMP:-}"
if [[ -z "${OBJDUMP}" ]]; then
    if command -v llvm-objdump >/dev/null 2>&1; then
        OBJDUMP="llvm-objdump"
    elif command -v objdump >/dev/null 2>&1; then
        OBJDUMP="objdump"
    else
        echo "ERROR: llvm-objdump or objdump is required for check-artifacts.sh" >&2
        exit 1
    fi
fi

check_windows_binary() {
    local bin="$1"
    local expected_arch="$2"

    echo "============================================================"
    echo "Checking Windows artifact: ${bin}"
    echo "============================================================"

    if [[ ! -f "${bin}" ]]; then
        echo "ERROR: File '${bin}' does not exist!" >&2
        return 1
    fi

    # 1. Check machine architecture
    echo "--- 1. Architecture Check (expected: ${expected_arch}) ---"
    local file_header
    file_header="$("${OBJDUMP}" -f "${bin}")"
    echo "${file_header}"

    if ! echo "${file_header}" | grep -q "${expected_arch}"; then
        echo "ERROR: Architecture mismatch in ${bin}! Expected ${expected_arch}" >&2
        return 1
    fi
    echo ">> Machine architecture matches: ${expected_arch}"

    # 2. Check imported DLLs
    echo "--- 2. Imported DLLs Check ---"
    local dlls=""
    if command -v llvm-readobj >/dev/null 2>&1; then
        dlls="$(llvm-readobj --coff-imports "${bin}" | grep -E '^\s*Name:\s*\S+\.dll' | awk '{print $2}' | sort -u)"
    else
        dlls="$("${OBJDUMP}" -p "${bin}" | grep -i "DLL Name:" | awk '{print $3}' | sort -u)"
    fi

    if [[ -z "${dlls}" ]]; then
        echo "ERROR: No imported DLLs found for ${bin}!" >&2
        return 1
    fi

    echo "Found imported DLLs:"
    echo "${dlls}" | sed 's/^/  - /'

    # Check for forbidden DLLs
    local forbidden_patterns='^(libgcc_s|libunwind|libwinpthread|libc\+\+|libstdc\+\+)'
    local forbidden_found=0

    while IFS= read -r dll; do
        [[ -z "${dll}" ]] && continue
        # Normalize to lowercase for pattern matching
        local lower_dll
        lower_dll="$(echo "${dll}" | tr '[:upper:]' '[:lower:]')"

        if echo "${lower_dll}" | grep -qE "${forbidden_patterns}"; then
            echo "ERROR: Forbidden runtime DLL imported: ${dll}" >&2
            forbidden_found=1
        fi

        # Check that it is an approved Windows system DLL
        local is_system=0
        if echo "${lower_dll}" | grep -qE '^(kernel32|advapi32|ole32|oleaut32|ntdll|userenv|ws2_32|shell32|user32|bcrypt|bcryptprimitives|iphlpapi|mmdevapi|propsys|avrt|combase|comctl32|dwmapi|dxgi|d3d12|d3dcompiler_47|gdi32|imm32|opengl32|rpcrt4|setupapi|uxtheme|shcore|winmm|version|dbghelp|crypt32|secur32|ncrypt|msvcrt|ucrtbase|api-ms-win-.*)\.dll$'; then
            is_system=1
        fi

        if [[ "${is_system}" -eq 0 ]]; then
            echo "ERROR: Non-system DLL imported: ${dll}" >&2
            forbidden_found=1
        fi
    done <<< "${dlls}"

    if [[ "${forbidden_found}" -ne 0 ]]; then
        echo "ERROR: Non-system DLLs found in ${bin}!" >&2
        return 1
    fi

    echo ">> All imported DLLs are verified Windows system DLLs."
    echo ""
}

check_linux_binary() {
    local bin="$1"
    local expected_arch="$2"

    echo "============================================================"
    echo "Checking Linux artifact: ${bin}"
    echo "============================================================"

    if [[ ! -f "${bin}" ]]; then
        echo "ERROR: File '${bin}' does not exist!" >&2
        return 1
    fi

    # 1. Check machine architecture via readelf
    echo "--- 1. Architecture Check (expected: ${expected_arch}) ---"
    if ! readelf -h "${bin}" 2>/dev/null | grep -qi "${expected_arch}"; then
        echo "ERROR: Architecture mismatch in ${bin}! Expected ${expected_arch}" >&2
        return 1
    fi
    echo ">> Machine architecture matches: ${expected_arch}"

    # 2. Check static linkage
    echo "--- 2. Static Linkage Check ---"
    if readelf -l "${bin}" 2>/dev/null | grep -q "INTERP"; then
        echo "ERROR: ${bin} is not fully static! Found INTERP program header." >&2
        return 1
    fi
    if readelf -d "${bin}" 2>/dev/null | grep -q "NEEDED"; then
        echo "ERROR: ${bin} is not fully static! Found NEEDED dynamic entries." >&2
        return 1
    fi
    echo ">> Fully static: no INTERP header and no NEEDED entries."
    echo ""
}

check_linux_packages() {
    if ! command -v bsdtar >/dev/null 2>&1; then
        echo "ERROR: bsdtar is required to inspect Linux packages." >&2
        return 1
    fi
    local version arch deb_arch rpm_arch arch_arch package listing path size
    version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"
    local expected=(
        usr/bin/nyx-refrain
        usr/bin/nyxr
        usr/share/applications/nyx-refrain.desktop
        usr/share/doc/nyx-refrain/linux-quickstart.md
        usr/share/licenses/nyx-refrain/LICENSE-MIT
        usr/share/licenses/nyx-refrain/LICENSE-APACHE
        usr/share/licenses/nyx-refrain/LICENSE-fluentui-emoji
        usr/share/doc/nyx-refrain/NOTICE
    )
    for size in 16 22 24 32 48 64 128 256; do
        expected+=("usr/share/icons/hicolor/${size}x${size}/apps/nyx-refrain.png")
    done
    for arch in x86_64 aarch64 loongarch64; do
        [[ -d "dist/linux-${arch}" ]] || continue
        case "${arch}" in
            x86_64) deb_arch=amd64; rpm_arch=x86_64; arch_arch=x86_64 ;;
            aarch64) deb_arch=arm64; rpm_arch=aarch64; arch_arch=aarch64 ;;
            loongarch64) deb_arch=loong64; rpm_arch=loongarch64; arch_arch=loong64 ;;
        esac
        for package in \
            "dist/packages/nyx-refrain_${version}-1_${deb_arch}.deb" \
            "dist/packages/nyx-refrain-${version}-1.${rpm_arch}.rpm" \
            "dist/packages/nyx-refrain-${version}-1-${arch_arch}.pkg.tar.zst"; do
            echo "Checking Linux package: ${package}"
            if [[ "${package}" == *.deb ]]; then
                listing="$(bsdtar -xOf "${package}" 'data.tar.*' | bsdtar -tf -)"
            else
                listing="$(bsdtar -tf "${package}")"
            fi
            listing="$(sed -e 's@^\./@@' -e 's@^/@@' <<< "${listing}")"
            for path in "${expected[@]}"; do
                if ! grep -Fxq "${path}" <<< "${listing}"; then
                    echo "ERROR: ${package} is missing ${path}" >&2
                    return 1
                fi
            done
            if [[ "${package}" == *.deb ]]; then
                grep -Fxq usr/share/doc/nyx-refrain/copyright <<< "${listing}"
            fi
            if grep -q '^etc/xdg/autostart/' <<< "${listing}"; then
                echo "ERROR: ${package} must not enable autostart." >&2
                return 1
            fi
        done
    done
}

if [[ $# -eq 0 ]]; then
    # Verify Windows artifacts if present
    for bin in dist/win32-amd64/*.exe; do
        [[ -f "$bin" ]] && check_windows_binary "$bin" "x86_64"
    done
    for bin in dist/win32-aarch64/*.exe; do
        [[ -f "$bin" ]] && check_windows_binary "$bin" "aarch64"
    done

    # Verify Linux artifacts if present
    for bin in dist/linux-x86_64/*; do
        [[ -f "$bin" && -x "$bin" ]] && check_linux_binary "$bin" "X86-64"
    done
    for bin in dist/linux-aarch64/*; do
        [[ -f "$bin" && -x "$bin" ]] && check_linux_binary "$bin" "AArch64"
    done
    for bin in dist/linux-loongarch64/*; do
        [[ -f "$bin" && -x "$bin" ]] && check_linux_binary "$bin" "LoongArch"
    done
    if [[ "${NO_PACKAGES:-0}" != 1 ]] && compgen -G 'dist/linux-*/nyxr' >/dev/null; then
        check_linux_packages
    fi
else
    # Fallback to positional check for custom invocations
    X86_BIN="${1:-dist/win32-amd64/nyxr.exe}"
    ARM_BIN="${2:-dist/win32-aarch64/nyxr.exe}"
    [[ -f "${X86_BIN}" ]] && check_windows_binary "${X86_BIN}" "x86_64"
    [[ -f "${ARM_BIN}" ]] && check_windows_binary "${ARM_BIN}" "aarch64"
fi

# Verify SHA256SUMS if present
if [[ -f dist/SHA256SUMS ]]; then
    echo "--- Checking dist/SHA256SUMS ---"
    (
        cd dist
        sha256sum --check --status SHA256SUMS
    )
    echo ">> SHA256SUMS verified successfully."
fi

echo "=== All artifacts passed verification ==="
