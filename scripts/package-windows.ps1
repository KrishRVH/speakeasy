$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
# GPUI compiles its release shaders with the Windows SDK compiler.
if (!$env:GPUI_FXC_PATH) {
    $compiler = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin/*/x64/fxc.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (!$compiler) { throw 'Install the Windows SDK (including fxc.exe) from Visual Studio Build Tools.' }
    $env:GPUI_FXC_PATH = $compiler.FullName
}
cargo build --release --locked -p speakeasy --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
$destination = Join-Path (Get-Location) 'artifacts/rust/windows'
New-Item -ItemType Directory -Force -Path $destination | Out-Null
Copy-Item 'target/x86_64-pc-windows-msvc/release/speakeasy.exe' $destination
Copy-Item 'settings.example.json' $destination
Compress-Archive -LiteralPath "$destination/speakeasy.exe", "$destination/settings.example.json" -DestinationPath 'artifacts/rust/speakeasy-windows-x64.zip' -Force
Write-Host "Built $destination"
