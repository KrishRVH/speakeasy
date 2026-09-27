param(
    [Parameter(Mandatory=$true)][string]$RuntimeDirectory,
    [Parameter(Mandatory=$true)][string]$Model,
    [string]$Executable = (Join-Path $PSScriptRoot '../artifacts/rust/windows/speakeasy.exe')
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
$modelPath = (Resolve-Path -LiteralPath $Model).Path
$app = (Resolve-Path -LiteralPath $Executable).Path
if (!(Test-Path -LiteralPath "$runtime/bin/nemo-speech.exe" -PathType Leaf) -or
    !(Test-Path -LiteralPath "$runtime/share/licenses" -PathType Container)) {
    throw 'RuntimeDirectory must contain the complete extracted NeMo-Speech.cpp Windows release.'
}
if ((Get-FileHash -LiteralPath $modelPath -Algorithm SHA256).Hash -ne
    'e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e') {
    throw 'Model must be the verified, unmodified NVIDIA Parakeet v3 q8 GGUF listed in README.md.'
}
$stage = Join-Path $env:TEMP ('speakeasy-package-' + [guid]::NewGuid().ToString('N'))
$bundle = Join-Path $stage 'Speakeasy'
$destination = Join-Path $root 'artifacts/rust/speakeasy-windows-parakeet-x64.zip'
try {
    $null = New-Item -ItemType Directory "$bundle/engine", "$bundle/models" -Force
    Copy-Item -LiteralPath $app -Destination "$bundle/speakeasy.exe"
    Copy-Item -LiteralPath "$runtime/bin" -Destination "$bundle/engine/bin" -Recurse
    Copy-Item -LiteralPath "$runtime/share/licenses" -Destination "$bundle/engine/licenses" -Recurse
    Copy-Item -LiteralPath $modelPath -Destination "$bundle/models/parakeet-v3-q8.gguf"
    Copy-Item -Path "$root/packaging/windows-portable/*" -Destination $bundle
    $settings = [ordered]@{
        engine = 'parakeet'
        engine_executable = 'engine/bin/nemo-speech.exe'
        model = 'models/parakeet-v3-q8.gguf'
        language = 'auto'
        threads = 1
        use_gpu = $true
        reduced_motion = $false
        preserve_clipboard = $false
    }
    [IO.File]::WriteAllText("$bundle/settings.json", ($settings | ConvertTo-Json))
    $null = New-Item -ItemType Directory (Split-Path $destination -Parent) -Force
    Compress-Archive -LiteralPath $bundle -DestinationPath $destination -Force
    Get-Item -LiteralPath $destination | Select-Object FullName, Length
    Get-FileHash -LiteralPath $destination -Algorithm SHA256
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
