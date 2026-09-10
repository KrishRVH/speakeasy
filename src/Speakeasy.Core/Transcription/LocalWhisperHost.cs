using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Net;
using System.Net.Http.Headers;
using System.Net.Sockets;
using System.Text.Json;
using Speakeasy.Core.Configuration;

namespace Speakeasy.Core.Transcription;

/// <summary>Keeps one owned Whisper model in GPU/CPU memory across dictations.</summary>
internal sealed class LocalWhisperHost : IDisposable
{
    private static readonly HttpClient LocalClient = new(new SocketsHttpHandler
    {
        AllowAutoRedirect = false,
        UseProxy = false,
        ConnectTimeout = TimeSpan.FromSeconds(2),
        PooledConnectionLifetime = TimeSpan.FromMinutes(5)
    })
    { Timeout = Timeout.InfiniteTimeSpan };

    private readonly TranscriptionSettings _options;
    private readonly string _directory;
    private readonly SemaphoreSlim _operation = new(1, 1);
    private readonly CancellationTokenSource _lifetime = new();
    private readonly object _gate = new();
    private Generation? _generation;
    private bool _disposed;

    internal LocalWhisperHost(TranscriptionSettings options, string directory)
    {
        _options = options;
        _directory = directory;
    }

    internal async Task WarmupAsync(CancellationToken cancellationToken)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, _lifetime.Token);
        await _operation.WaitAsync(lifetime.Token).ConfigureAwait(false);
        try { await EnsureStartedAsync(lifetime.Token).ConfigureAwait(false); }
        finally { _operation.Release(); }
    }

    internal async Task<string> TranscribeAsync(byte[] waveData, CancellationToken cancellationToken)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, _lifetime.Token);
        var token = lifetime.Token;
        await _operation.WaitAsync(token).ConfigureAwait(false);
        try
        {
            var generation = await EnsureStartedAsync(token).ConfigureAwait(false);
            // Cancelling an HTTP request alone does not reliably stop GPU kernels in older
            // whisper.cpp releases. Kill only the generation that received this recording.
            using var cancel = token.Register(generation.Stop);
            using var request = new HttpRequestMessage(HttpMethod.Post, new Uri(generation.Endpoint, "inference"));
            using var form = new MultipartFormDataContent();
            var file = new ByteArrayContent(waveData);
            file.Headers.ContentType = new MediaTypeHeaderValue("audio/wav");
            form.Add(file, "file", "dictation.wav");
            form.Add(new StringContent("json"), "response_format");
            form.Add(new StringContent("0.0"), "temperature");
            form.Add(new StringContent("true"), "no_context");
            form.Add(new StringContent("false"), "carry_initial_prompt");
            request.Content = form;
            try
            {
                using var response = await LocalClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, token).ConfigureAwait(false);
                if (!response.IsSuccessStatusCode)
                    throw new TranscriptionException($"Local Whisper transcription failed (HTTP {(int)response.StatusCode}). Try recording again.");
                await response.Content.LoadIntoBufferAsync(1024 * 1024, token).ConfigureAwait(false);
                await using var stream = await response.Content.ReadAsStreamAsync(token).ConfigureAwait(false);
                using var data = await JsonDocument.ParseAsync(stream, cancellationToken: token).ConfigureAwait(false);
                token.ThrowIfCancellationRequested();
                if (data.RootElement.ValueKind != JsonValueKind.Object ||
                    !data.RootElement.TryGetProperty("text", out var text) || text.ValueKind != JsonValueKind.String)
                    throw new TranscriptionException("Local Whisper returned no transcript. Check the installed whisper.cpp version.");
                return text.GetString()!;
            }
            catch
            {
                generation.Stop();
                token.ThrowIfCancellationRequested();
                throw;
            }
        }
        finally { _operation.Release(); }
    }

    private async Task<Generation> EnsureStartedAsync(CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (_generation is { Alive: true }) return _generation;
            _generation?.Dispose();
            _generation = null;
        }
        var executable = Path.GetFullPath(_options.WhisperServerExecutable, _directory);
        var model = Path.GetFullPath(_options.ModelPath, _directory);
        if (!File.Exists(executable))
            throw new TranscriptionException("The whisper.cpp server is missing. Run setup-local.ps1 or set transcription.whisperServerExecutable.");
        if (!File.Exists(model))
            throw new TranscriptionException("The local Whisper model is missing. Run setup-local.ps1 or set transcription.modelPath.");

        // Reserve an ephemeral loopback port while constructing the process. The server
        // binds immediately after release; an unguessable path identifies this instance.
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        var instance = "s-" + Guid.NewGuid().ToString("N");
        var publicDirectory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Speakeasy", "Temp", instance);
        Directory.CreateDirectory(publicDirectory);
        var start = new ProcessStartInfo(executable)
        {
            WorkingDirectory = Path.GetDirectoryName(executable)!,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardError = true,
            RedirectStandardOutput = true
        };
        foreach (var argument in new[]
        {
            "--model", model, "--host", "127.0.0.1", "--port", port.ToString(CultureInfo.InvariantCulture),
            "--request-path", "/" + instance, "--public", publicDirectory,
            "--threads", _options.Threads.ToString(CultureInfo.InvariantCulture),
            "--language", string.IsNullOrWhiteSpace(_options.Language) ? "auto" : _options.Language,
            "--no-timestamps"
        }) start.ArgumentList.Add(argument);
        if (!_options.UseGpu) start.ArgumentList.Add("--no-gpu");
        var process = new Process { StartInfo = start };
        Generation generation;
        try
        {
            if (!process.Start()) throw new TranscriptionException("The local Whisper server could not be started.");
            generation = new Generation(process, new Uri($"http://127.0.0.1:{port}/{instance}/"), publicDirectory);
        }
        catch (Exception exception) when (exception is Win32Exception or TranscriptionException)
        {
            process.Dispose();
            TryRemoveDirectory(publicDirectory);
            throw new TranscriptionException("The local Whisper server could not start. Check the executable, its DLLs, and the GPU driver.");
        }
        lock (_gate)
        {
            if (_disposed)
            {
                generation.Dispose();
                throw new OperationCanceledException(token);
            }
            _generation = generation;
        }
        using var startup = CancellationTokenSource.CreateLinkedTokenSource(token);
        startup.CancelAfter(TimeSpan.FromSeconds(_options.StartupTimeoutSeconds));
        using var abortStartup = startup.Token.Register(generation.Stop);
        try
        {
            while (true)
            {
                startup.Token.ThrowIfCancellationRequested();
                if (!generation.Alive)
                    throw new TranscriptionException("The local Whisper server stopped while loading. Check the model, CUDA DLLs, and available memory.");
                try
                {
                    using var poll = CancellationTokenSource.CreateLinkedTokenSource(startup.Token);
                    poll.CancelAfter(TimeSpan.FromSeconds(2));
                    using var response = await LocalClient.GetAsync(new Uri(generation.Endpoint, "health"), poll.Token).ConfigureAwait(false);
                    if (response.IsSuccessStatusCode)
                    {
                        var payload = await response.Content.ReadAsStringAsync(poll.Token).ConfigureAwait(false);
                        using var data = JsonDocument.Parse(payload);
                        if (data.RootElement.ValueKind == JsonValueKind.Object &&
                            data.RootElement.TryGetProperty("status", out var status) &&
                            status.ValueKind == JsonValueKind.String && status.GetString() == "ok")
                            return generation;
                    }
                    else if (response.StatusCode == HttpStatusCode.NotFound)
                    {
                        // Older whisper.cpp builds load synchronously and expose only '/'.
                        using var fallback = await LocalClient.GetAsync(generation.Endpoint, poll.Token).ConfigureAwait(false);
                        var body = await fallback.Content.ReadAsStringAsync(poll.Token).ConfigureAwait(false);
                        if (fallback.IsSuccessStatusCode && body.Contains("Whisper.cpp Server", StringComparison.Ordinal))
                            return generation;
                    }
                }
                catch (HttpRequestException) { }
                catch (JsonException) { }
                catch (OperationCanceledException) when (!startup.IsCancellationRequested) { }
                await Task.Delay(100, startup.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) when (!token.IsCancellationRequested)
        {
            generation.Stop();
            throw new TranscriptionException("Loading the local Whisper model timed out. Check GPU support or increase transcription.startupTimeoutSeconds.");
        }
        catch
        {
            generation.Stop();
            throw;
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed) return;
            _disposed = true;
            _lifetime.Cancel();
            _generation?.Dispose();
            _generation = null;
        }
        // In-flight operations still own linked tokens and the semaphore until unwound.
    }

    private static void TryRemoveDirectory(string path)
    {
        try { Directory.Delete(path, recursive: true); }
        catch (IOException) { }
        catch (UnauthorizedAccessException) { }
    }

    private sealed class Generation : IDisposable
    {
        private readonly Process _process;
        private readonly string _publicDirectory;
        private readonly CancellationTokenSource _streams = new();
        private readonly Task _drain;
        private int _stopped;
        private int _disposed;
        internal Uri Endpoint { get; }
        internal bool Alive
        {
            get
            {
                if (Volatile.Read(ref _stopped) != 0) return false;
                try { return !_process.HasExited; }
                catch (InvalidOperationException) { return false; }
            }
        }

        internal Generation(Process process, Uri endpoint, string publicDirectory)
        {
            _process = process;
            Endpoint = endpoint;
            _publicDirectory = publicDirectory;
            _drain = DrainAsync();
        }

        private async Task DrainAsync()
        {
            try
            {
                await Task.WhenAll(
                    _process.StandardOutput.BaseStream.CopyToAsync(Stream.Null, _streams.Token),
                    _process.StandardError.BaseStream.CopyToAsync(Stream.Null, _streams.Token)).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { }
            catch (IOException) { }
            catch (ObjectDisposedException) { }
        }

        internal void Stop()
        {
            if (Interlocked.Exchange(ref _stopped, 1) != 0) return;
            try { if (!_process.HasExited) _process.Kill(entireProcessTree: true); }
            catch (InvalidOperationException) { }
            catch (Win32Exception) { }
            _streams.Cancel();
        }

        public void Dispose()
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0) return;
            Stop();
            _process.Dispose();
            // Stream completion owns CTS disposal to avoid a race with pending reads.
            _ = _drain.ContinueWith(_ => _streams.Dispose(), TaskScheduler.Default);
            TryRemoveDirectory(_publicDirectory);
        }
    }
}
