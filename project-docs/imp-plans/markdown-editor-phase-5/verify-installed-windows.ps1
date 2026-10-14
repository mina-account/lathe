param([Parameter(Mandatory=$true)][string]$ArchivePath,[string]$AcceptanceRoot='C:/tmp/lathe-phase5-installed-windows')
$ErrorActionPreference='Stop'
Set-Location (Join-Path $PSScriptRoot '../../..')
$acceptanceRoot=$AcceptanceRoot
$installation="$acceptanceRoot/application"
New-Item -ItemType Directory -Path $acceptanceRoot -Force | Out-Null
& script/install-fork-windows.ps1 -ArchivePath $ArchivePath -InstallDirectory $installation -NoShellIntegration
$executable="$installation/libexec/lathe-editor.exe"
$savedPath=$env:Path
try {
  $env:Path="$env:SystemRoot/System32;$env:SystemRoot"
  $env:CEF_PATH=$null
  $env:LATHE_MARKDOWN_CEF_RUNTIME=$null
  $env:HTTP_PROXY='http://127.0.0.1:9'
  $env:HTTPS_PROXY='http://127.0.0.1:9'
  $env:ALL_PROXY='http://127.0.0.1:9'
  $env:NO_PROXY=$null
  if (Get-Command node -ErrorAction SilentlyContinue) {throw 'Node remains on acceptance PATH'}
  $runs=@()
  foreach($name in @('normal','missing-runtime')) {
    $runRoot="$acceptanceRoot/$name"
    $configuration="$runRoot/user-data/config"
    New-Item -ItemType Directory -Path $configuration -Force | Out-Null
    [IO.File]::WriteAllText("$configuration/settings.json",'{"auto_update":false,"disable_ai":true,"auto_install_extensions":{"html":false},"language_servers":[],"languages":{"Markdown":{"language_servers":[],"formatter":"none","prettier":{"allowed":false}}},"markdown_editor":{"default_open_mode":"editor","mode":"instant_rendering","plantuml_service":null},"markdown_preview":{"open_markdown_files_in_preview":true}}',[Text.UTF8Encoding]::new($false))
    Copy-Item crates/markdown_editor/installed-fixture.md "$runRoot/fixture.md" -Force
    $report="$runRoot/report.json"
    if (Test-Path $report) {throw "Acceptance report already exists: $report"}
    $env:LATHE_MARKDOWN_INSTALLATION_CHECK=$report
    if ($name -eq 'missing-runtime') {
      $emptyRuntime="$runRoot/incomplete-runtime"
      New-Item -ItemType Directory -Path $emptyRuntime -Force | Out-Null
      $env:LATHE_MARKDOWN_CEF_RUNTIME=$emptyRuntime
    }
    $process=Start-Process -FilePath $executable -ArgumentList @('--foreground','--user-data-dir',"$runRoot/user-data","$runRoot/fixture.md") -WorkingDirectory "$installation/libexec" -WindowStyle Hidden -PassThru -RedirectStandardOutput "$runRoot/stdout.log" -RedirectStandardError "$runRoot/stderr.log"
    $retainedHandle=$process.Handle
    $owned=@{}
    $deadline=[DateTime]::UtcNow.AddSeconds(180)
    do {
      Get-CimInstance Win32_Process -Filter "Name='lathe-editor.exe'" | Where-Object {$_.ExecutablePath -eq [IO.Path]::GetFullPath($executable)} | ForEach-Object {
        $owned[$_.ProcessId]=@{process_id=$_.ProcessId;parent_process_id=$_.ParentProcessId;executable=$_.ExecutablePath;command_line=$_.CommandLine}
      }
      Start-Sleep -Milliseconds 500
      $process.Refresh()
    } while(-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline)
    if (-not $process.HasExited) {throw "Installed process $($process.Id) timed out; retained for diagnosis"}
    $process.WaitForExit()
    if ($null -eq $process.ExitCode -or $process.ExitCode -ne 0) {throw "Installed process exit was not zero: $($process.ExitCode), handle $retainedHandle"}
    if (-not (Test-Path $report)) {throw "Installed $name exited $($process.ExitCode) without report"}
    $result=Get-Content -Raw -Encoding UTF8 $report | ConvertFrom-Json
    if ($name -eq 'normal') {
      if (-not $result.success -or $result.modes.Count -ne 4 -or -not $result.source_preserved) {throw "Installed renderer check failed: $(ConvertTo-Json $result -Depth 20)"}
      if (-not ($owned.Values | Where-Object {$_.command_line -match '--type='})) {throw 'Installed CEF helper subprocess was not observed'}
    } else {
      if ($result.success -or -not $result.source_preserved -or $result.error -notmatch 'Reinstall') {throw 'Missing runtime did not preserve source and explain reinstall'}
    }
    $runs+=@{name=$name;result=$result;exit_code=$process.ExitCode;processes=@($owned.Values);fixture_sha256=(Get-FileHash "$runRoot/fixture.md" -Algorithm SHA256).Hash}
  }
  @{archive=$ArchivePath;archive_sha256=(Get-FileHash $ArchivePath -Algorithm SHA256).Hash;executable=$executable;executable_sha256=(Get-FileHash $executable -Algorithm SHA256).Hash;node_on_path=$false;proxy='http://127.0.0.1:9';cef_override_normal=$null;runs=$runs} | ConvertTo-Json -Depth 30 | Set-Content -Encoding UTF8 "$acceptanceRoot/acceptance.json"
  Write-Output 'INSTALLED_WINDOWS_CHECKS_PASSED'
} finally {
  $env:Path=$savedPath
}
