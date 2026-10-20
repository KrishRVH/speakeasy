$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
# GPUI compiles its release shaders with the Windows SDK compiler.
if (!$env:GPUI_FXC_PATH) {
    $compiler = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin/*/x64/fxc.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (!$compiler) { throw 'Install the Windows SDK (including fxc.exe) from Visual Studio Build Tools.' }
    $env:GPUI_FXC_PATH = $compiler.FullName
}
# VCRUNTIME140.dll comes from the Visual C++ Redistributable, not Windows, so link the CRT statically.
# With --target set, the flag reaches only target crates; cc-built C code follows it to /MT.
$callerRustFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = '-C target-feature=+crt-static'
try {
    cargo build --release --locked -p speakeasy --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
} finally {
    $env:RUSTFLAGS = $callerRustFlags
}
$destination = Join-Path (Get-Location) 'artifacts/windows'
New-Item -ItemType Directory -Force -Path $destination | Out-Null
Copy-Item 'target/x86_64-pc-windows-msvc/release/speakeasy.exe' $destination
$image = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("$destination/speakeasy.exe"))
if ($image.IndexOf('VCRUNTIME140', [StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw 'speakeasy.exe imports the Visual C++ runtime, which Windows does not include'
}
Copy-Item 'settings.example.json' $destination
Copy-Item 'crates/app/assets/JosefinSans-OFL.txt' $destination
Copy-Item 'crates/show/NOTICE.md' $destination
Compress-Archive -LiteralPath "$destination/speakeasy.exe", "$destination/settings.example.json", "$destination/JosefinSans-OFL.txt", "$destination/NOTICE.md" -DestinationPath 'artifacts/speakeasy-windows-x64.zip' -Force
Write-Host "Built $destination"
