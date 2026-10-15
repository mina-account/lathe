[CmdletBinding()]
Param(
    [Parameter()][string]$ArchivePath,
    [Parameter()][string]$InstallDirectory,
    [Parameter()][switch]$NoShellIntegration
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Set-Location (Join-Path $PSScriptRoot '..')

$channel = (Get-Content 'crates/zed/RELEASE_CHANNEL').Trim()
$suffix = if ($channel -ne 'stable') { "-$channel" } else { '' }

$appName = switch ($channel) {
    'stable'  { 'Lathe' }
    'preview' { 'Lathe Preview' }
    'beta'    { 'Lathe Beta' }
    'nightly' { 'Lathe Nightly' }
    default   { 'Lathe Dev' }
}

# --- Find archive ---
if (-not $ArchivePath) {
    $candidates = Get-ChildItem -Path 'target/release' -Filter 'Lathe-*-windows.zip' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending
    if (-not $candidates) {
        Write-Error "No Windows zip found in target/release/. Run 'script/build-fork-windows.ps1 && script/package-fork-windows.ps1' first."
        exit 1
    }
    $ArchivePath = $candidates[0].FullName
}

if (-not (Test-Path $ArchivePath)) {
    Write-Error "Archive not found: $ArchivePath"
    exit 1
}

# --- Install location ---
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\Lathe'
$installDir  = if ($InstallDirectory) { [IO.Path]::GetFullPath($InstallDirectory) } else { Join-Path $installRoot "lathe$suffix" }
$binDir      = Join-Path $installDir 'bin'

Write-Output "=== Installing $appName ==="
Write-Output "  From: $ArchivePath"
Write-Output "  To:   $installDir"

# --- Detect a running instance and ask the user before overwriting ---
$running = Get-Process -Name 'lathe-editor', 'lathe' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq (Join-Path $installDir 'libexec/lathe-editor.exe') -or $_.Path -eq (Join-Path $installDir 'bin/lathe.exe') }
if ($running) {
    Write-Output ""
    Write-Output "An existing Lathe process is running. Close it before installing, or it will be terminated."
    $reply = Read-Host "Terminate running Lathe processes? [y/N]"
    if ($reply -match '^[Yy]') {
        $running | Stop-Process -Force
        Start-Sleep -Seconds 1
    } else {
        Write-Error "Install aborted."
        exit 1
    }
}

# --- Extract via temp dir, then move contents into installDir ---
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
try {
    Expand-Archive -Path $ArchivePath -DestinationPath $tempDir -Force
    Get-ChildItem -Path $tempDir -Recurse -File | Unblock-File

    # The zip wraps everything in `lathe${suffix}/` -- flatten one level.
    $inner = Join-Path $tempDir "lathe$suffix"
    if (-not (Test-Path $inner)) {
        Write-Error "Archive layout unexpected: missing 'lathe$suffix/' at root of $ArchivePath"
        exit 1
    }
    $runtimeDirectory = Join-Path $inner 'libexec'
    $manifestPath = Join-Path $runtimeDirectory 'markdown-runtime.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Markdown runtime manifest is missing. Reinstall a complete Lathe package.' }
    foreach ($file in @('libcef.dll','icudtl.dat','resources.pak','chrome_100_percent.pak','chrome_200_percent.pak','v8_context_snapshot.bin','locales/en-US.pak','CEF-LICENSE.txt','markdown-editor-notices/NOTICES.md')) {
        if (-not (Test-Path -LiteralPath (Join-Path $runtimeDirectory $file) -PathType Leaf)) { throw "Markdown runtime missing $file. Reinstall a complete Lathe package." }
    }
    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($manifest.cef -ne '154.0.34' -or $manifest.bindings -ne '154.5.0') { throw 'The archive has an incompatible Markdown runtime.' }
    foreach ($file in $manifest.files) {
        $runtimeFile = [IO.Path]::GetFullPath((Join-Path $runtimeDirectory $file.path))
        if (-not $runtimeFile.StartsWith(([IO.Path]::GetFullPath($runtimeDirectory) + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid runtime manifest path.' }
        if ((Get-FileHash -LiteralPath $runtimeFile -Algorithm SHA256).Hash -ne $file.sha256) { throw "Markdown runtime checksum mismatch: $($file.path)" }
    }
    if ([IO.Path]::GetPathRoot($installDir) -eq $installDir) { throw 'Cannot install at a filesystem root.' }
    if (Test-Path -LiteralPath $installDir) {
        Write-Output "Removing existing installation..."
        Remove-Item -LiteralPath $installDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Get-ChildItem -Path $inner -Force | ForEach-Object {
        Move-Item -Path $_.FullName -Destination $installDir -Force
    }
}
finally {
    $resolvedTemporaryDirectory = [IO.Path]::GetFullPath($tempDir)
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolvedTemporaryDirectory.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or $resolvedTemporaryDirectory -eq $temporaryRoot) { throw 'Unsafe installer temporary directory.' }
    Remove-Item -LiteralPath $resolvedTemporaryDirectory -Recurse -Force
}

if ($NoShellIntegration) {
    Write-Output "Installed $appName at $installDir without shell integration."
    return
}

# --- Add bin/ to user PATH (idempotent) ---
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathEntries = if ($userPath) { $userPath -split ';' } else { @() }
if ($pathEntries -notcontains $binDir) {
    $newPath = if ($userPath) { "$userPath;$binDir" } else { $binDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Output "Added $binDir to user PATH (open a new terminal to pick it up)."
} else {
    Write-Output "$binDir already on user PATH."
}

# --- Start Menu shortcut ---
$startMenuDir = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$shortcutPath = Join-Path $startMenuDir "$appName.lnk"
$editorExe = Join-Path $installDir 'libexec\lathe-editor.exe'
$iconPath = Join-Path $installDir 'share\lathe.ico'

try {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $editorExe
    $shortcut.WorkingDirectory = $installDir
    if (Test-Path $iconPath) { $shortcut.IconLocation = $iconPath }
    $shortcut.Save()
    Write-Output "Created Start Menu shortcut: $shortcutPath"
} catch {
    Write-Output "Warning: Could not create Start Menu shortcut: $_"
}

Write-Output ""
Write-Output "=== Installed ==="
Write-Output "  App: $installDir"
Write-Output "  CLI: $binDir\lathe.exe"
Write-Output ""
Write-Output "Launch from Start Menu, or open a new terminal and run:"
Write-Output "  lathe ."
Write-Output ""
Write-Output "Note: This fork installs as '$appName' and runs alongside stock Zed."
