param([string]$SetupScriptPath = (Join-Path $PSScriptRoot '../scripts/setup-local.ps1'))

$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$testDirectory = Join-Path $repositoryRoot ('artifacts/setup-download-tests/' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDirectory -Force | Out-Null

# Load only the download function. Running setup itself would install software
# and edit local settings, which these simulated network tests must not do.
$parseTokens = $null
$parseErrors = $null
$scriptAst = [System.Management.Automation.Language.Parser]::ParseFile(
    $SetupScriptPath, [ref]$parseTokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) { throw ($parseErrors | Out-String) }
$downloadFunction = $scriptAst.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-HuggingFaceDownload'
}, $true)
if (-not $downloadFunction) { throw 'The model download function was not found.' }
. ([scriptblock]::Create($downloadFunction.Extent.Text))

$script:ModelBytes = [Text.Encoding]::UTF8.GetBytes('A complete model fixture.')
$hasher = [Security.Cryptography.SHA256]::Create()
try { $script:ModelHash = [BitConverter]::ToString($hasher.ComputeHash($script:ModelBytes)).Replace('-', '') }
finally { $hasher.Dispose() }
$script:Behavior = 'normal'
$script:FullCalls = 0
$script:Ranges = @()

function Invoke-RestMethod {
    param([string]$Uri, [int]$TimeoutSec)
    if ($Uri -ne 'https://huggingface.co/api/models/fixture/model/tree/main') { throw 'Unexpected metadata URL.' }
    [pscustomobject]@{ path = 'model.bin'; size = $script:ModelBytes.Length; lfs = [pscustomobject]@{ oid = $script:ModelHash } }
}

function curl.exe {
    $arguments = @($args)
    $outputPath = $arguments[[Array]::IndexOf($arguments, '--output') + 1]
    $rangeIndex = [Array]::IndexOf($arguments, '--range')
    if ($rangeIndex -lt 0) {
        $script:FullCalls++
        if ($script:Behavior -in @('fallback', 'bad-range')) {
            $global:LASTEXITCODE = 28
            return
        }
        $payload = [byte[]]$script:ModelBytes.Clone()
        if ($script:Behavior -eq 'bad-hash') { $payload[0] = 0 }
        [IO.File]::WriteAllBytes($outputPath, $payload)
    } else {
        $range = [string]$arguments[$rangeIndex + 1]
        $script:Ranges += $range
        $bounds = $range.Split('-')
        $first = [int]$bounds[0]
        $last = [int]$bounds[1]
        [IO.File]::WriteAllBytes($outputPath, [byte[]]$script:ModelBytes[$first..$last])
        $headersPath = $arguments[[Array]::IndexOf($arguments, '--dump-header') + 1]
        $reportedFirst = if ($script:Behavior -eq 'bad-range') { $first + 1 } else { $first }
        "HTTP/1.1 206 Partial Content`r`nContent-Range: bytes $reportedFirst-$last/$($script:ModelBytes.Length)`r`n" |
            Set-Content -LiteralPath $headersPath
    }
    $global:LASTEXITCODE = 0
}

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$uri = 'https://huggingface.co/fixture/model/resolve/main/model.bin'
$normal = Join-Path $testDirectory 'normal.bin'
Get-HuggingFaceDownload $uri $normal
Assert-Condition ($script:FullCalls -eq 1 -and $script:Ranges.Count -eq 0) 'Normal downloads must not make range requests.'
Assert-Condition ((Get-FileHash -LiteralPath $normal -Algorithm SHA256).Hash -eq $script:ModelHash) 'Normal download contents were changed.'

$script:Behavior = 'fallback'
$resume = Join-Path $testDirectory 'resume.bin'
[IO.File]::WriteAllBytes(($resume + '.partial'), [byte[]]$script:ModelBytes[0..3])
Get-HuggingFaceDownload $uri $resume
Assert-Condition ($script:Ranges[-1] -eq "4-$($script:ModelBytes.Length - 1)") 'Fallback must preserve completed bytes and resume at the correct offset.'
Assert-Condition ((Get-FileHash -LiteralPath $resume -Algorithm SHA256).Hash -eq $script:ModelHash) 'Resumed contents do not match the expected model.'

$script:Behavior = 'bad-range'
$wrongRange = Join-Path $testDirectory 'wrong-range.bin'
[IO.File]::WriteAllBytes(($wrongRange + '.partial'), [byte[]]$script:ModelBytes[0..3])
$rejectedRange = $false
try { Get-HuggingFaceDownload $uri $wrongRange }
catch { $rejectedRange = $_.Exception.Message -like '*unexpected byte range*' }
Assert-Condition $rejectedRange 'An incorrect Content-Range must be rejected.'
Assert-Condition ((Get-Item -LiteralPath ($wrongRange + '.partial')).Length -eq 4) 'An incorrect range must not corrupt the existing partial file.'
Assert-Condition (-not (Test-Path -LiteralPath $wrongRange)) 'An incorrect range must not produce an installed model.'

$script:Behavior = 'bad-hash'
$wrongHash = Join-Path $testDirectory 'wrong-hash.bin'
$rejectedHash = $false
try { Get-HuggingFaceDownload $uri $wrongHash }
catch { $rejectedHash = $_.Exception.Message -like '*checksum did not match*' }
Assert-Condition $rejectedHash 'A corrupted full download must fail its official checksum.'
Assert-Condition (-not (Test-Path -LiteralPath $wrongHash)) 'A corrupted download must not produce an installed model.'

$settingsFunction = $scriptAst.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Read-SetupSettings'
}, $true)
if (-not $settingsFunction) { throw 'Setup cannot read valid commented settings and omitted defaults.' }
. ([scriptblock]::Create($settingsFunction.Extent.Text))
$settingsFixture = Join-Path $testDirectory 'settings.json'
@'
{
  // Keep my shortcut when installing another local model.
  "hotkey": "F8",
  "transcription": {
    "modelPath": "models/a//b,}/*literal*/\\model.bin",
    "language": "auto", /* The model may be multilingual. */
  },
}
'@ | Set-Content -LiteralPath $settingsFixture
$templatePath = Join-Path $repositoryRoot 'settings.example.json'
$settings = Read-SetupSettings $settingsFixture $templatePath
Assert-Condition ($settings.hotkey -eq 'F8') 'Setup must preserve customized settings.'
Assert-Condition ($settings.transcription.modelPath -eq 'models/a//b,}/*literal*/\model.bin') 'Comment and comma syntax inside quoted values must remain literal.'
Assert-Condition ($settings.transcription.language -eq 'auto') 'Existing nested settings must override defaults.'
Assert-Condition ($settings.transcription.threads -eq 4 -and $settings.cleanup.provider -eq 'auto' -and $settings.microphoneDevice -eq -1) 'Omitted nested and root settings must receive the shipped defaults.'

'{"transcription":null}' | Set-Content -LiteralPath $settingsFixture
$rejectedNullSection = $false
try { Read-SetupSettings $settingsFixture $templatePath | Out-Null }
catch { $rejectedNullSection = $_.Exception.Message -like '*transcription*JSON object*' }
Assert-Condition $rejectedNullSection 'An explicitly invalid provider section must not silently become default settings.'

Write-Output 'Passed 6 setup scenarios: normal download, resumed fallback, incorrect range, incorrect hash, commented/default settings, invalid section.'
