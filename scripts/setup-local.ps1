[CmdletBinding()]
param(
    [string]$ConfigDirectory = (Join-Path $env:APPDATA 'speakeasy'),
    [ValidateSet('small.en', 'small', 'medium.en', 'medium')][string]$Model = 'small.en',
    [ValidateSet('cpu', 'cuda')][string]$Backend = 'cpu',
    [switch]$IncludeCleanup
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$configRoot = [IO.Path]::GetFullPath($ConfigDirectory)
$downloads = Join-Path $configRoot 'downloads'
$models = Join-Path $configRoot 'models'
New-Item -ItemType Directory -Path $configRoot, $downloads, $models -Force | Out-Null

function Get-HuggingFaceDownload([string]$Uri, [string]$Path) {
    # Try a normal resumable download first. Some networks stall on large
    # responses; bounded ranges recover without discarding completed bytes.
    if ($Uri -notmatch '^https://huggingface\.co/([^/]+/[^/]+)/resolve/([^/]+)/([^?]+)') {
        throw "Unsupported Hugging Face model URL: $Uri"
    }
    $repository = $Matches[1]
    $revision = $Matches[2]
    $fileName = $Matches[3]
    $entries = Invoke-RestMethod -Uri "https://huggingface.co/api/models/$repository/tree/$revision" -TimeoutSec 30
    $model = $entries | Where-Object { $_.path -eq $fileName } | Select-Object -First 1
    if (-not $model -or $model.size -le 0 -or $model.lfs.oid -notmatch '^[a-fA-F0-9]{64}$') {
        throw "Could not read the official size and SHA256 for $fileName. Try setup again."
    }
    $expectedSize = [long]$model.size
    $expectedHash = [string]$model.lfs.oid
    $partial = $Path + '.partial'
    $chunk = $Path + '.chunk'
    $headers = $Path + '.headers'
    $offset = if (Test-Path -LiteralPath $partial) { (Get-Item -LiteralPath $partial).Length } else { 0L }
    if ($offset -gt $expectedSize) { throw "The partial model is too large: $partial. Remove it and run setup again." }
    $chunkSize = 64MB
    $separator = if ($Uri.Contains('?')) { '&' } else { '?' }

    if ($offset -lt $expectedSize) {
        $fullUri = $Uri + $separator + 'download=true'
        & curl.exe --fail --location --silent --show-error --connect-timeout 15 --max-time 1800 --speed-time 30 --speed-limit 1024 --continue-at - --output $partial $fullUri
        if ($LASTEXITCODE -eq 0) {
            $offset = (Get-Item -LiteralPath $partial).Length
            if ($offset -ne $expectedSize) { throw "The downloaded model size does not match Hugging Face metadata: $partial" }
        } else {
            $offset = if (Test-Path -LiteralPath $partial) { (Get-Item -LiteralPath $partial).Length } else { 0L }
            if ($offset -gt $expectedSize) { throw "The partial model is too large: $partial. Remove it and run setup again." }
            Write-Host '  Full download interrupted. Resuming with bounded byte ranges.'
        }
    }

    while ($offset -lt $expectedSize) {
        $last = [Math]::Min($offset + $chunkSize - 1, $expectedSize - 1)
        # A distinct resolver URL also avoids reusing an expired cached redirect.
        $rangeUri = $Uri + $separator + 'download=true&speakeasyRange=' + $offset
        & curl.exe --fail --location --silent --show-error --connect-timeout 15 --max-time 300 --speed-time 30 --speed-limit 1024 --retry 3 --retry-all-errors --range "$offset-$last" --dump-header $headers --output $chunk $rangeUri
        if ($LASTEXITCODE -ne 0) { throw "Model download stopped at byte $offset. Run setup again to resume: $Uri" }
        $rangeHeader = Get-Content -LiteralPath $headers | Where-Object { $_ -match '^Content-Range:' } | Select-Object -Last 1
        $expectedRange = "bytes $offset-$last/$expectedSize"
        if (-not $rangeHeader -or $rangeHeader.Trim() -notmatch ('(?i)^Content-Range:\s*' + [regex]::Escape($expectedRange) + '$') -or
            (Get-Item -LiteralPath $chunk).Length -ne ($last - $offset + 1)) {
            throw "The server returned an unexpected byte range for $fileName. Run setup again to resume."
        }

        $destinationStream = [IO.File]::Open($partial, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        try {
            $sourceStream = [IO.File]::OpenRead($chunk)
            try { $sourceStream.CopyTo($destinationStream) }
            finally { $sourceStream.Dispose() }
        }
        finally { $destinationStream.Dispose() }
        $offset = $last + 1
        Write-Host ('  {0:P0} ({1:N0} / {2:N0} bytes)' -f ($offset / $expectedSize), $offset, $expectedSize)
    }

    if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash -ne $expectedHash) {
        throw "The model checksum did not match Hugging Face metadata: $partial. Remove it and run setup again."
    }
    Move-Item -LiteralPath $partial -Destination $Path
    Remove-Item -LiteralPath $chunk, $headers -Force -ErrorAction SilentlyContinue
}

function Get-Download([string]$Uri, [string]$Path) {
    if (Test-Path -LiteralPath $Path) { return }
    Write-Host ('Downloading ' + [IO.Path]::GetFileName($Path))
    if ($Uri.StartsWith('https://huggingface.co/')) {
        Get-HuggingFaceDownload $Uri $Path
        return
    }
    $partial = $Path + '.partial'
    & curl.exe --fail --location --silent --show-error --connect-timeout 15 --max-time 1800 --speed-time 30 --speed-limit 1024 --retry 3 --retry-all-errors --continue-at - --output $partial $Uri
    if ($LASTEXITCODE -ne 0) { throw "Download failed: $Uri" }
    Move-Item -LiteralPath $partial -Destination $Path -Force
}

function Install-Archive([string]$Archive, [string]$Target, [string]$Executable) {
    New-Item -ItemType Directory -Path $Target -Force | Out-Null
    Expand-Archive -LiteralPath $Archive -DestinationPath $Target -Force
    $found = Get-ChildItem -LiteralPath $Target -Filter $Executable -File -Recurse | Select-Object -First 1
    if (-not $found) { throw "$Executable was not found in $Archive" }
    return $found.FullName
}

$whisperTag = 'b4938'
$whisperAsset = if ($Backend -eq 'cuda') { 'whisper-cublas-12.4.0-bin-x64.zip' } else { 'whisper-bin-x64.zip' }
$whisperZip = Join-Path $downloads $whisperAsset
Get-Download "https://github.com/ggml-org/whisper.cpp/releases/download/$whisperTag/$whisperAsset" $whisperZip
$whisperExe = Install-Archive $whisperZip (Join-Path $configRoot 'tools/whisper') 'whisper-cli.exe'
$whisperServer = Join-Path (Split-Path -Parent $whisperExe) 'whisper-server.exe'
$modelPath = Join-Path $models "ggml-$Model.bin"
Get-Download "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$Model.bin" $modelPath

$settingsPath = Join-Path $configRoot 'settings.json'
if (Test-Path -LiteralPath $settingsPath) {
    $settings = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
} else {
    $template = Join-Path $PSScriptRoot '../settings.example.json'
    $settings = Get-Content -LiteralPath $template -Raw | ConvertFrom-Json
}
$settings.transcription | Add-Member -NotePropertyName provider -NotePropertyValue 'local' -Force
$settings.transcription | Add-Member -NotePropertyName whisperExecutable -NotePropertyValue $whisperExe -Force
$settings.transcription | Add-Member -NotePropertyName whisperServerExecutable -NotePropertyValue $whisperServer -Force
$settings.transcription | Add-Member -NotePropertyName localMode -NotePropertyValue 'server' -Force
$settings.transcription | Add-Member -NotePropertyName modelPath -NotePropertyValue $modelPath -Force
$settings.transcription | Add-Member -NotePropertyName useGpu -NotePropertyValue ($Backend -eq 'cuda') -Force
$settings.transcription | Add-Member -NotePropertyName threads -NotePropertyValue ([Math]::Min(8, [Environment]::ProcessorCount)) -Force

if ($IncludeCleanup) {
    $llamaTag = 'b10809'
    $llamaAsset = if ($Backend -eq 'cuda') { "llama-$llamaTag-bin-win-cuda-12.4-x64.zip" } else { "llama-$llamaTag-bin-win-cpu-x64.zip" }
    $llamaZip = Join-Path $downloads $llamaAsset
    Get-Download "https://github.com/ggml-org/llama.cpp/releases/download/$llamaTag/$llamaAsset" $llamaZip
    $llamaExe = Install-Archive $llamaZip (Join-Path $configRoot 'tools/llama') 'llama-server.exe'
    if ($Backend -eq 'cuda') {
        $runtimeAsset = 'cudart-llama-bin-win-cuda-12.4-x64.zip'
        $runtimeZip = Join-Path $downloads $runtimeAsset
        Get-Download "https://github.com/ggml-org/llama.cpp/releases/download/$llamaTag/$runtimeAsset" $runtimeZip
        Expand-Archive -LiteralPath $runtimeZip -DestinationPath (Split-Path -Parent $llamaExe) -Force
    }
    $cleanupModel = Join-Path $models 'qwen2.5-3b-instruct-q4_k_m.gguf'
    Get-Download 'https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf' $cleanupModel
    $settings.cleanup | Add-Member -NotePropertyName provider -NotePropertyValue 'local' -Force
    $settings.cleanup | Add-Member -NotePropertyName model -NotePropertyValue 'qwen2.5-3b' -Force
    $settings.cleanup | Add-Member -NotePropertyName autoStartLocal -NotePropertyValue $true -Force
    $settings.cleanup | Add-Member -NotePropertyName localExecutable -NotePropertyValue $llamaExe -Force
    $settings.cleanup | Add-Member -NotePropertyName localModelPath -NotePropertyValue $cleanupModel -Force
    $settings.cleanup | Add-Member -NotePropertyName gpuLayers -NotePropertyValue $(if ($Backend -eq 'cuda') { 99 } else { 0 }) -Force
}

$settings | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $settingsPath -Encoding utf8
$envPath = Join-Path $configRoot '.env'
if (-not (Test-Path -LiteralPath $envPath)) {
    '# Optional: OPENAI_API_KEY= or GROQ_API_KEY=. Local dictation needs no key.' | Set-Content -LiteralPath $envPath -Encoding utf8
}
Write-Host "Local dictation is installed. Settings: $settingsPath"
Write-Host 'If speakeasy is running, choose Reload settings in its tray menu.'
