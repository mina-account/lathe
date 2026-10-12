param(
    [Parameter(Mandatory = $true)][string]$RuntimeDirectory,
    [Parameter(Mandatory = $true)][string]$ReportPath
)
$ErrorActionPreference = 'Stop'
$runtimePath = [System.IO.Path]::GetFullPath($RuntimeDirectory)
foreach ($required in @('markdown_offscreen_probe.exe', 'libcef.dll', 'icudtl.dat', 'resources.pak', 'locales')) {
    if (-not (Test-Path -LiteralPath (Join-Path $runtimePath $required))) {
        throw "Missing CEF probe runtime component: $required. Build the locked offscreen feature and retain its copied runtime files."
    }
}
$reportFile = [System.IO.Path]::GetFullPath($ReportPath)
$reportDirectory = Split-Path -Parent $reportFile
[System.IO.Directory]::CreateDirectory($reportDirectory) | Out-Null
$env:LATHE_PROBE_OUTPUT = Join-Path $reportDirectory 'scene-images'
$probeProcess = Start-Process -FilePath (Join-Path $runtimePath 'markdown_offscreen_probe.exe') -ArgumentList '--smoke' -WindowStyle Hidden -RedirectStandardOutput $reportFile -RedirectStandardError "$reportFile.stderr" -PassThru
$probeProcess.Handle | Out-Null
if (-not $probeProcess.WaitForExit(35000)) {
    Stop-Process -Id $probeProcess.Id
    throw 'Offscreen browser smoke exceeded 35 seconds.'
}
if ($null -eq $probeProcess.ExitCode) { throw 'The probe exit status could not be determined.' }
if ($probeProcess.ExitCode -ne 0) {
    throw "Offscreen browser smoke failed with exit $($probeProcess.ExitCode). Read $reportFile.stderr."
}
Write-Output "Offscreen browser smoke passed: $reportFile"
