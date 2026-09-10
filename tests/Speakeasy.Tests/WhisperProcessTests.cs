using System.Diagnostics;
using System.Text;
using Speakeasy.Core.Transcription;

namespace Speakeasy.Tests;

public sealed class WhisperProcessTests
{
    [Fact]
    public async Task CancellationTerminatesHangingProcessPromptly()
    {
        if (!OperatingSystem.IsWindows()) return;
        var start = new ProcessStartInfo(Path.Combine(Environment.SystemDirectory, "cmd.exe"));
        start.ArgumentList.Add("/d");
        start.ArgumentList.Add("/c");
        start.ArgumentList.Add("ping -n 60 127.0.0.1 >nul");
        using var cancellation = new CancellationTokenSource(TimeSpan.FromMilliseconds(200));
        var clock = Stopwatch.StartNew();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => WhisperProcess.RunAsync(start, cancellation.Token));
        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(5), "Cancellation should not wait for the child process to finish.");
    }

    [Fact]
    public async Task CancellationKillsTheOwnedDescendantAsWellAsItsParent()
    {
        if (!OperatingSystem.IsWindows()) return;
        var marker = Path.Combine(Path.GetTempPath(), "speakeasy-process-" + Guid.NewGuid().ToString("N") + ".txt");
        var executable = Path.Combine(Environment.SystemDirectory, "WindowsPowerShell", "v1.0", "powershell.exe");
        var child = Convert.ToBase64String(Encoding.Unicode.GetBytes("Start-Sleep -Seconds 60"));
        var command = $$"""
            $child = Start-Process -FilePath '{{executable.Replace("'", "''")}}' -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{{child}}' -PassThru -WindowStyle Hidden
            [System.IO.File]::WriteAllText('{{marker.Replace("'", "''")}}', "$PID,$($child.Id)")
            Start-Sleep -Seconds 60
            """;
        var start = new ProcessStartInfo(executable);
        foreach (var argument in new[] { "-NoProfile", "-NonInteractive", "-EncodedCommand", Convert.ToBase64String(Encoding.Unicode.GetBytes(command)) })
            start.ArgumentList.Add(argument);
        using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(15));
        var run = WhisperProcess.RunAsync(start, cancellation.Token);
        try
        {
            while (!File.Exists(marker))
            {
                cancellation.Token.ThrowIfCancellationRequested();
                await Task.Delay(20, cancellation.Token);
            }
            var ids = (await File.ReadAllTextAsync(marker, cancellation.Token)).Split(',').Select(int.Parse).ToArray();
            Assert.Equal(2, ids.Length);
            cancellation.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => run);
            foreach (var id in ids)
            {
                try
                {
                    using var process = Process.GetProcessById(id);
                    await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(3));
                    Assert.True(process.HasExited);
                }
                catch (ArgumentException) { /* Windows has already removed the process. */ }
            }
        }
        finally
        {
            cancellation.Cancel();
            try { await run; }
            catch (OperationCanceledException) { }
            File.Delete(marker);
        }
    }

    [Fact]
    public async Task BothOutputPipesAreDrainedWithoutDeadlocking()
    {
        if (!OperatingSystem.IsWindows()) return;
        var start = new ProcessStartInfo(Path.Combine(Environment.SystemDirectory, "cmd.exe"));
        start.ArgumentList.Add("/d");
        start.ArgumentList.Add("/c");
        start.ArgumentList.Add("for /L %i in (1,1,12000) do @(echo pipeline-test-output & echo pipeline-test-error 1>&2)");
        using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(15));
        Assert.Equal(0, await WhisperProcess.RunAsync(start, cancellation.Token));
    }
}
