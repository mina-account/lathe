[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ArchivePath)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('lathe-update-smoke-' + [Guid]::NewGuid())
$editor = $null
$failure = $null
$cleanupFailure = $null
$previousStateless = $env:ZED_STATELESS
$env:ZED_STATELESS = '1'
try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $testRoot
    $installation = Get-ChildItem -LiteralPath $testRoot -Directory | Select-Object -First 1
    if (-not $installation) { throw 'Missing archive installation directory.' }
    $installRoot = $installation.FullName
    $profile = Join-Path $testRoot 'profile'
    New-Item -ItemType Directory -Path (Join-Path $profile 'config') -Force | Out-Null
    $settings = Join-Path $profile 'config/settings.json'
    [IO.File]::WriteAllText($settings, '{"auto_update":false,"agent":{"super_compact_mode":true}}', [Text.UTF8Encoding]::new($false))
    $settingsHash = (Get-FileHash -LiteralPath $settings).Hash
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $initialMarker = Get-Content -LiteralPath (Join-Path $installRoot 'release.txt')
    $coreVersion = [version]$initialMarker[1]
    $channel = $initialMarker[2]
    $architecture = $initialMarker[3]
    $editorPath = Join-Path $installRoot 'libexec/lathe-editor.exe'
    $compiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe'
    if (-not (Test-Path -LiteralPath $compiler)) {
        $compiler = Join-Path $env:LOCALAPPDATA 'Programs/Inno Setup 6/ISCC.exe'
    }
    if (-not (Test-Path -LiteralPath $compiler)) { throw 'Inno Setup compiler is required for update smoke.' }
    for ($iteration = 1; $iteration -le 2; $iteration++) {
        $nextVersion = "$($coreVersion.Major).$($coreVersion.Minor).$($coreVersion.Build + $iteration)"
        # CI checks the transaction protocol using real binaries and fixture marker versions.
        # Delivery acceptance separately builds genuinely different application versions.
        & $compiler /Qp "/DLatheChannel=$channel" "/DLatheVersion=$nextVersion" "/DLatheArch=$architecture" "/DStageDir=$installRoot" "/DOutputDir=$testRoot" (Join-Path $PSScriptRoot '../crates/zed/resources/windows/lathe.iss')
        if ($LASTEXITCODE -ne 0) { throw 'Fixture installer compilation failed.' }
        $editor = Start-Process -FilePath $editorPath -ArgumentList @('--user-data-dir', ('"' + $profile + '"')) -WindowStyle Hidden -PassThru
        Start-Sleep -Seconds 10
        if ($editor.HasExited) { throw 'Isolated editor exited before staging.' }
        $logPath = Join-Path $profile 'logs/Lathe.log'
        if (-not (Test-Path -LiteralPath $logPath)) { throw 'Missing isolated profile log; user-data-dir was not honored.' }
        $startupLog = [IO.File]::ReadAllText($logPath)
        if ($startupLog.Contains('Failed to load user settings')) { throw 'Isolated settings were rejected; refusing to stage an update.' }
        if ($startupLog.Contains('Auto Update: checking for updates')) { throw 'Automatic polling ran despite the isolated opt-out.' }
        if (-not $startupLog.Contains("starting lathe version $coreVersion")) { throw 'Wrong application binary version in isolated startup log.' }
        $before = (Get-FileHash -LiteralPath $editorPath).Hash
        $installer = Join-Path $testRoot "Lathe-$nextVersion-$architecture-windows-setup.exe"
        $stage = Start-Process -FilePath $installer -ArgumentList @('/VERYSILENT', '/update=true', '/NOCLOSEAPPLICATIONS', ('/DIR="' + $installRoot + '"')) -WindowStyle Hidden -PassThru -Wait
        if ($stage.ExitCode -ne 0) { throw "Staging failed: $($stage.ExitCode)" }
        if ($editor.HasExited) { throw 'Background staging closed the editor.' }
        if ((Get-FileHash -LiteralPath $editorPath).Hash -ne $before) { throw 'Background staging replaced the running editor.' }
        $marker = Get-Content -LiteralPath (Join-Path $installRoot 'install/release.txt')
        if ($marker[0] -ne 'lathe-update-v1' -or $marker[1] -ne $nextVersion -or $marker[2] -ne $channel -or $marker[3] -ne $architecture) { throw 'Invalid staging marker.' }
        $updates = Join-Path $installRoot 'updates'
        New-Item -ItemType Directory -Path $updates -Force | Out-Null
        $helper = Join-Path $updates 'auto_update_helper.exe'
        Copy-Item -LiteralPath (Join-Path $installRoot 'tools/auto_update_helper.exe') -Destination $helper
        if (-not $editor.CloseMainWindow()) { throw 'Cannot request normal quit of isolated editor.' }
        if (-not $editor.WaitForExit(30000)) { throw 'Normal quit did not finish.' }
        $editor = $null
        $deadline = [DateTime]::UtcNow.AddSeconds(60)
        while ($true) {
            $installedMarker = Join-Path $installRoot 'release.txt'
            if (Test-Path -LiteralPath $installedMarker) {
                $installedVersion = (Get-Content -LiteralPath $installedMarker)[1]
                if ($installedVersion -eq $nextVersion) { break }
            }
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Graceful quit did not finish applying the update.' }
            Start-Sleep -Milliseconds 250
        }
        $installed = Get-Content -LiteralPath (Join-Path $installRoot 'release.txt')
        if ($installed[1] -ne $nextVersion) { throw 'Update helper did not apply target release.' }
        if ((Get-FileHash -LiteralPath $settings).Hash -ne $settingsHash) { throw 'Settings changed during update.' }
        if ([Environment]::GetEnvironmentVariable('Path', 'User') -ne $userPath) { throw 'Update mode changed user PATH.' }
        & (Join-Path $installRoot 'bin/lathe.exe') --version
        if ($LASTEXITCODE -ne 0) { throw 'CLI is unusable after update.' }
        Write-Output "Transaction $iteration passed (fixture marker $nextVersion)."
    }
}
catch {
    $failure = $_
    Write-Output "Update smoke failed before cleanup: $($_.Exception.Message)"
}
finally {
    $env:ZED_STATELESS = $previousStateless
    try {
        if ($editor -and -not $editor.HasExited) {
            if (-not $editor.CloseMainWindow() -or -not $editor.WaitForExit(10000)) {
                Stop-Process -Id $editor.Id -Force
                $editor.WaitForExit()
            }
        }
        $resolved = [IO.Path]::GetFullPath($testRoot)
        $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        if (-not $resolved.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Smoke cleanup escaped temporary root.' }
        $ownedPrefix = $resolved.TrimEnd('\') + '\'
        $ownedProcesses = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($ownedPrefix, [StringComparison]::OrdinalIgnoreCase) }
        foreach ($ownedProcess in $ownedProcesses) {
            $process = Get-Process -Id $ownedProcess.ProcessId -ErrorAction SilentlyContinue
            if ($process -and -not $process.WaitForExit(10000)) {
                Stop-Process -Id $process.Id -Force
                $process.WaitForExit()
            }
        }
        $cleanupDeadline = [DateTime]::UtcNow.AddSeconds(15)
        while (Test-Path -LiteralPath $resolved) {
            try { Remove-Item -LiteralPath $resolved -Recurse -Force }
            catch {
                if ([DateTime]::UtcNow -ge $cleanupDeadline) { throw }
                Start-Sleep -Milliseconds 250
            }
        }
    }
    catch {
        $cleanupFailure = $_
        Write-Warning "Update smoke cleanup failed: $($_.Exception.Message)"
    }
}
if ($failure) { throw $failure }
if ($cleanupFailure) { throw $cleanupFailure }
