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
foreach ($executable in @('speakeasy.exe', 'speakeasy-cli.exe')) {
    Copy-Item "target/x86_64-pc-windows-msvc/release/$executable" $destination
    $image = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("$destination/$executable"))
    if ($image.IndexOf('VCRUNTIME140', [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "$executable imports the Visual C++ runtime, which Windows does not include"
    }
}
$version = & "$destination/speakeasy-cli.exe" --version
if ($LASTEXITCODE -ne 0 -or $version -notmatch '^speakeasy \d+\.\d+\.\d+$') {
    throw 'The console entry did not return its version and exit status to PowerShell'
}
Copy-Item 'settings.example.json' $destination
Copy-Item 'LICENSE' $destination
cargo about generate --locked --fail -c packaging/about.toml --target x86_64-pc-windows-msvc -o "$destination/THIRD-PARTY-NOTICES.txt" packaging/notices.hbs
if ($LASTEXITCODE -ne 0) { throw 'Third-party notices failed' }
Copy-Item 'crates/app/assets/JosefinSans-OFL.txt' $destination
Copy-Item 'crates/show/NOTICE.md' $destination
Compress-Archive -LiteralPath "$destination/speakeasy.exe", "$destination/speakeasy-cli.exe", "$destination/settings.example.json", "$destination/LICENSE", "$destination/THIRD-PARTY-NOTICES.txt", "$destination/JosefinSans-OFL.txt", "$destination/NOTICE.md" -DestinationPath 'artifacts/speakeasy-windows-x64.zip' -Force
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (!$iscc) {
    $iscc = (Get-Item "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe" -ErrorAction SilentlyContinue).FullName
}
if (!$iscc) { throw 'Install Inno Setup 6 to build the per-user installer. The portable ZIP has been built.' }
$appVersion = $version.Trim().Substring('speakeasy '.Length)
& $iscc "/DAppVersion=$appVersion" "/DPackageRoot=$destination" 'packaging/windows/installer.iss'
if ($LASTEXITCODE -ne 0) { throw 'Windows installer compilation failed' }
Write-Host "Built $destination"
