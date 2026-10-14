$ErrorActionPreference = 'Stop'
$frontendDirectory = Join-Path $PSScriptRoot '../crates/markdown_editor/frontend'
Push-Location $frontendDirectory
try {
    & npm.cmd ci --ignore-scripts
    if ($LASTEXITCODE -ne 0) { throw 'Locked Markdown frontend installation failed.' }
    & node build.mjs
    if ($LASTEXITCODE -ne 0) { throw 'Markdown frontend build failed; Node.js 24 or newer is required.' }
} finally {
    Pop-Location
}
& node (Join-Path $PSScriptRoot '../project-docs/imp-plans/markdown-editor-phase-4/verify-assets.mjs')
if ($LASTEXITCODE -ne 0) { throw 'Pinned Markdown asset verification failed.' }
