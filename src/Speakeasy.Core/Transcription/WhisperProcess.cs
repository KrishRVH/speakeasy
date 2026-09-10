using System.ComponentModel;
using System.Diagnostics;

namespace Speakeasy.Core.Transcription;

internal static class WhisperProcess
{
    // Both pipes are drained without accumulating model logs or dictated text in memory.
    internal static async Task<int> RunAsync(ProcessStartInfo startInfo, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        startInfo.UseShellExecute = false;
        startInfo.CreateNoWindow = true;
        startInfo.RedirectStandardOutput = true;
        startInfo.RedirectStandardError = true;
        using var process = new Process { StartInfo = startInfo };
        try
        {
            if (!process.Start()) throw new TranscriptionException("whisper.cpp could not be started.");
        }
        catch (Win32Exception)
        {
            throw new TranscriptionException("whisper.cpp could not be started. Check the executable, its DLLs, and your CPU or GPU build.");
        }
        using var stopRegistration = cancellationToken.Register(() => KillTree(process));
        var output = process.StandardOutput.BaseStream.CopyToAsync(Stream.Null, cancellationToken);
        var error = process.StandardError.BaseStream.CopyToAsync(Stream.Null, cancellationToken);
        try
        {
            await process.WaitForExitAsync(cancellationToken).ConfigureAwait(false);
            await Task.WhenAll(output, error).ConfigureAwait(false);
            cancellationToken.ThrowIfCancellationRequested();
            return process.ExitCode;
        }
        finally
        {
            KillTree(process);
            // Let Windows release the WAV before the caller deletes the temporary directory.
            try { await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)).ConfigureAwait(false); }
            catch (TimeoutException) { }
            // Observe cancelled stream tasks as well as successful drains.
            try { await Task.WhenAll(output, error).ConfigureAwait(false); }
            catch (OperationCanceledException) { }
            catch (IOException) { }
        }
    }

    private static void KillTree(Process process)
    {
        try
        {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
        }
        catch (InvalidOperationException) { }
        catch (Win32Exception) { }
    }
}
