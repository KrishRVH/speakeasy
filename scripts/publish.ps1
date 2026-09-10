[CmdletBinding()]
param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/publish'))

$ErrorActionPreference = 'Stop'
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)
$project = Join-Path $repoRoot 'src/Speakeasy.App/Speakeasy.App.csproj'
& dotnet publish $project -c Release -r win-x64 --self-contained true -p:PublishReadyToRun=true -p:NuGetLockFilePath=obj/publish.packages.lock.json -p:RestoreLockedMode=false -o $outputRoot
if ($LASTEXITCODE -ne 0) { throw 'Publishing failed.' }

foreach ($name in @('README.md', 'settings.example.json', '.env.example')) {
    Copy-Item -LiteralPath (Join-Path $repoRoot $name) -Destination $outputRoot -Force
}
Copy-Item -LiteralPath (Join-Path $repoRoot 'docs') -Destination $outputRoot -Recurse -Force
$scriptTarget = Join-Path $outputRoot 'scripts'
New-Item -ItemType Directory -Path $scriptTarget -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'setup-local.ps1') -Destination $scriptTarget -Force
$zip = Join-Path $repoRoot 'artifacts/speakeasy-win-x64.zip'
Compress-Archive -Path (Join-Path $outputRoot '*') -DestinationPath $zip -Force
Write-Host "App: $(Join-Path $outputRoot 'speakeasy.exe')"
Write-Host "Archive: $zip"
