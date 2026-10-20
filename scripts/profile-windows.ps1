param(
    [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int[]]$ProcessIds,
    [Parameter(Mandatory)][string]$Label,
    [ValidateRange(1, 3600)][int]$Seconds = 30,
    [ValidateRange(100, 10000)][int]$IntervalMs = 1000,
    [Parameter(Mandatory)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
# GPU memory counters exist only where Windows publishes them under these names; their absence
# leaves the metrics out rather than failing the run. Raw values read at once, where Get-Counter
# would wait a second for each.
function Get-GpuMemory([int]$ProcessId) {
    $usage = @{}
    try {
        $category = [Diagnostics.PerformanceCounterCategory]::new('GPU Process Memory')
        $instances = @($category.GetInstanceNames() | Where-Object { $_ -like "pid_${ProcessId}_*" })
        foreach ($kind in @('Dedicated', 'Shared')) {
            $total = [long]0
            foreach ($instance in $instances) {
                $counter = [Diagnostics.PerformanceCounter]::new('GPU Process Memory', "$kind Usage", $instance, $true)
                try { $total += $counter.RawValue } finally { $counter.Dispose() }
            }
            $usage[$kind] = $total
        }
    } catch {}
    $usage
}
$clock = [System.Diagnostics.Stopwatch]::StartNew()
$logicalCpus = [Environment]::ProcessorCount
$results = @{}
$previous = @{}
foreach ($processId in ($ProcessIds | Select-Object -Unique)) {
    # JSON objects need string keys; Windows PowerShell refuses to serialize numeric ones.
    $results[[string]$processId] = @{ samples = [System.Collections.Generic.List[object]]::new(); end = $null }
}
while ($true) {
    foreach ($processId in @($results.Keys)) {
        $result = $results[$processId]
        if ($null -ne $result.end) { continue }
        $process = $null
        try {
            $process = Get-Process -Id ([int]$processId)
            $process.Refresh()
            $elapsed = $clock.Elapsed.TotalSeconds
            $sample = @{
                elapsed_seconds = $elapsed
                identity = $process.StartTime.ToUniversalTime().Ticks
                cpu_seconds = $process.TotalProcessorTime.TotalSeconds
                rss_bytes = $process.WorkingSet64
                peak_rss_bytes = $process.PeakWorkingSet64
                private_bytes = $process.PrivateMemorySize64
                virtual_bytes = $process.VirtualMemorySize64
                threads = $process.Threads.Count
                handles = $process.HandleCount
            }
            $gpu = Get-GpuMemory ([int]$processId)
            if ($gpu.ContainsKey('Dedicated')) { $sample.gpu_dedicated_bytes = $gpu.Dedicated }
            if ($gpu.ContainsKey('Shared')) { $sample.gpu_shared_bytes = $gpu.Shared }
            $before = $previous[$processId]
            if ($null -ne $before) {
                if ($before.identity -ne $sample.identity) { $result.end = 'PID reused'; continue }
                $sample.cpu_one_core_percent = 100 * ($sample.cpu_seconds - $before.cpu_seconds) / ($elapsed - $before.elapsed_seconds)
                $sample.cpu_machine_percent = $sample.cpu_one_core_percent / $logicalCpus
            }
            $previous[$processId] = $sample
            $result.samples.Add($sample)
        } catch { $result.end = $_.Exception.GetType().Name }
        finally { if ($null -ne $process) { $process.Dispose() } }
    }
    $remainingMs = $Seconds * 1000 - $clock.Elapsed.TotalMilliseconds
    if ($remainingMs -le 0 -or @($results.Values | Where-Object { $null -eq $_.end }).Count -eq 0) { break }
    Start-Sleep -Milliseconds ([int][Math]::Min($IntervalMs, $remainingMs))
}
foreach ($result in $results.Values) {
    $summary = @{}
    foreach ($metric in @('cpu_one_core_percent', 'cpu_machine_percent', 'rss_bytes', 'private_bytes', 'gpu_dedicated_bytes', 'gpu_shared_bytes', 'threads', 'handles')) {
        $values = @($result.samples | Where-Object { $_.ContainsKey($metric) } | ForEach-Object { $_[$metric] } | Sort-Object)
        if ($values.Count -eq 0) { continue }
        $middle = [int][Math]::Floor($values.Count / 2)
        $median = if ($values.Count % 2) { $values[$middle] } else { ($values[$middle - 1] + $values[$middle]) / 2 }
        $summary[$metric] = @{ median = $median; p95 = $values[[int][Math]::Ceiling(0.95 * $values.Count) - 1]; max = $values[-1] }
    }
    if ($result.samples.Count -ge 2) {
        $first = $result.samples[0]
        $last = $result.samples[$result.samples.Count - 1]
        $elapsed = $last.elapsed_seconds - $first.elapsed_seconds
        $cpu = $last.cpu_seconds - $first.cpu_seconds
        $summary.cpu_time_seconds = $cpu
        $summary.average_cpu_one_core_percent = 100 * $cpu / $elapsed
        $summary.average_cpu_machine_percent = 100 * $cpu / $elapsed / $logicalCpus
    }
    $result.summary = $summary
}
$report = @{ label = $Label; logical_cpus = $logicalCpus; duration_seconds = $clock.Elapsed.TotalSeconds; processes = $results }
$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force $parent | Out-Null }
$report | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $OutputPath
Write-Output "Wrote $OutputPath; 100% CPU means one logical core."
