# Run in Windows PowerShell from a local checkout, not inside WSL.
# Prerequisites: Node.js 22+, Rust stable MSVC, Visual Studio C++ Build Tools,
# Windows 10/11 SDK and WebView2 Runtime (included with Windows 11).
param([switch]$Dev)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$env:RUST_BACKTRACE = '1'
foreach ($tool in @('node', 'npm', 'cargo')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Missing prerequisite: $tool. Install it and reopen PowerShell."
    }
}
& npm.cmd ci
if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
if ($Dev) {
    & npm.cmd run tauri dev
} else {
    & npm.cmd run tauri build
}
if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed' }
