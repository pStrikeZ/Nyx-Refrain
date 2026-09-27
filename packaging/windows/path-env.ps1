# Adds or removes a directory in the machine PATH (used by the NSIS installer).
#
# NSIS strings are limited to 1024 characters, so editing PATH inside the installer could
# truncate a long PATH. This script reads the raw value without expanding %VARIABLES%,
# keeps the registry value kind (REG_EXPAND_SZ stays expandable), and only writes when the
# directory is actually added or removed.
param(
    [Parameter(Mandatory = $true)][ValidateSet('add', 'remove')][string]$Action,
    [Parameter(Mandatory = $true)][string]$Dir
)
$ErrorActionPreference = 'Stop'

$key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey(
    'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', $true)
try {
    $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
    if ($null -ne $key.GetValue('Path')) { $kind = $key.GetValueKind('Path') }

    function Normalize([string]$p) {
        [Environment]::ExpandEnvironmentVariables($p).Trim().TrimEnd('\').ToLowerInvariant()
    }
    $target = Normalize $Dir
    $parts = @($raw -split ';' | Where-Object { $_.Trim() -ne '' })
    $rest = @($parts | Where-Object { (Normalize $_) -ne $target })
    $present = $rest.Count -ne $parts.Count

    if ($Action -eq 'add') {
        if ($present) { exit 0 }
        $new = if ($raw.TrimEnd().EndsWith(';') -or $raw.Trim() -eq '') { $raw.TrimEnd() + $Dir } else { $raw + ';' + $Dir }
    } else {
        if (-not $present) { exit 0 }
        $new = $rest -join ';'
    }
    $key.SetValue('Path', $new, $kind)
} finally {
    $key.Close()
}
