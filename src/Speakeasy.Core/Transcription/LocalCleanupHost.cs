using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using Speakeasy.Core.Configuration;

namespace Speakeasy.Core.Transcription;

internal sealed record LocalCleanupLease(Uri Endpoint, Action Abort);

/// <summary>Owns a warm llama.cpp worker. Leases pin cancellation to a particular process.</summary>
internal sealed class LocalCleanupHost(CleanupSettings options, string configDirectory) : IDisposable
{
    private static readonly HttpClient HealthClient = new(new SocketsHttpHandler { UseProxy = false, AllowAutoRedirect = false })
    { Timeout = TimeSpan.FromSeconds(2) };
    private readonly SemaphoreSlim _startup = new(1, 1);
    private readonly CancellationTokenSource _lifetime = new();
    private readonly object _gate = new();
    private Worker? _worker;
    private bool _disposed;

    public async Task WarmupAsync(CancellationToken cancellationToken) =>
        _ = await GetLeaseAsync(cancellationToken).ConfigureAwait(false);

    public async Task<LocalCleanupLease> GetLeaseAsync(CancellationToken cancellationToken)
    {
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, _lifetime.Token);
        linked.CancelAfter(TimeSpan.FromMinutes(2));
        var token = linked.Token;
        await _startup.WaitAsync(token).ConfigureAwait(false);
        try
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            Worker? existing;
            lock (_gate)
            {
                existing = _worker;
                if (existing is { Ready: true, Stopped: false } && !existing.Process.HasExited)
                    return new(existing.Endpoint, () => Abort(existing));
            }

            if (existing is not null) Abort(existing);
            var executable = Path.GetFullPath(options.LocalExecutable, configDirectory);
            var model = Path.GetFullPath(options.LocalModelPath, configDirectory);
            if (!File.Exists(executable) || !File.Exists(model))
                throw new TranscriptionException("Local cleanup needs llama.cpp and a model. Run setup-local.ps1 -IncludeCleanup.");
            var listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            var port = ((IPEndPoint)listener.LocalEndpoint).Port;
            listener.Stop();
            var start = new ProcessStartInfo(executable)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardError = true,
                RedirectStandardOutput = true,
                WorkingDirectory = Path.GetDirectoryName(executable)!
            };
            foreach (var argument in new[]
            {
                "--model", model, "--host", "127.0.0.1", "--port", port.ToString(CultureInfo.InvariantCulture),
                "--ctx-size", "8192", "--parallel", "1", "--n-gpu-layers", options.GpuLayers.ToString(CultureInfo.InvariantCulture),
                "--threads", Math.Min(8, Environment.ProcessorCount).ToString(CultureInfo.InvariantCulture),
                "--alias", string.IsNullOrWhiteSpace(options.Model) ? "local" : options.Model,
                "--no-webui"
            }) start.ArgumentList.Add(argument);
            var process = new Process { StartInfo = start };
            try { process.Start(); }
            catch (Win32Exception)
            {
                process.Dispose();
                throw new TranscriptionException("Local cleanup could not start. Check the llama.cpp executable and its CUDA DLLs.");
            }
            var worker = new Worker(process, new Uri($"http://127.0.0.1:{port}/v1"));
            worker.Output = process.StandardOutput.BaseStream.CopyToAsync(Stream.Null);
            worker.Error = process.StandardError.BaseStream.CopyToAsync(Stream.Null);
            lock (_gate)
            {
                if (_disposed) { Abort(worker); throw new OperationCanceledException(token); }
                _worker = worker;
            }
            using var cancelled = token.Register(() => Abort(worker));
            try
            {
                while (true)
                {
                    token.ThrowIfCancellationRequested();
                    if (process.HasExited)
                        throw new TranscriptionException("Local cleanup stopped during startup. Check model compatibility and GPU memory.");
                    try
                    {
                        using var response = await HealthClient.GetAsync($"http://127.0.0.1:{port}/health", token).ConfigureAwait(false);
                        if (response.IsSuccessStatusCode) break;
                    }
                    catch (HttpRequestException) { }
                    catch (OperationCanceledException) when (!token.IsCancellationRequested) { }
                    await Task.Delay(120, token).ConfigureAwait(false);
                }
                token.ThrowIfCancellationRequested();
                worker.Ready = true;
                return new(worker.Endpoint, () => Abort(worker));
            }
            catch { Abort(worker); throw; }
        }
        finally { _startup.Release(); }
    }

    private void Abort(Worker worker)
    {
        lock (_gate)
        {
            if (worker.Stopped) return;
            worker.Stopped = true;
            if (ReferenceEquals(_worker, worker)) _worker = null;
            try { if (!worker.Process.HasExited) worker.Process.Kill(entireProcessTree: true); }
            catch (InvalidOperationException) { }
            catch (Win32Exception) { }
            _ = ReapAsync(worker);
        }
    }

    private static async Task ReapAsync(Worker worker)
    {
        try
        {
            await worker.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)).ConfigureAwait(false);
            await Task.WhenAll(worker.Output, worker.Error).ConfigureAwait(false);
        }
        catch (Exception exception) when (exception is InvalidOperationException or IOException or TimeoutException) { }
        finally { worker.Process.Dispose(); }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed) return;
            _disposed = true;
            _lifetime.Cancel();
            if (_worker is { } worker) Abort(worker);
        }
    }

    private sealed class Worker(Process process, Uri endpoint)
    {
        public Process Process { get; } = process;
        public Uri Endpoint { get; } = endpoint;
        public bool Ready { get; set; }
        public bool Stopped { get; set; }
        public Task Output { get; set; } = Task.CompletedTask;
        public Task Error { get; set; } = Task.CompletedTask;
    }
}
