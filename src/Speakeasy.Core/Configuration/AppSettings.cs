namespace Speakeasy.Core.Configuration;

public sealed class AppSettings
{
    public string Hotkey { get; set; } = "Ctrl+Alt+Space";
    public bool Enabled { get; set; } = true;
    public int DoubleTapMs { get; set; } = 300;
    public int TapMaxMs { get; set; } = 220;
    public int MaxRecordingSeconds { get; set; } = 300;
    public bool RestoreClipboard { get; set; }
    public int ClipboardRestoreDelayMs { get; set; } = 800;
    public int MicrophoneDevice { get; set; } = -1;
    public double SilenceThreshold { get; set; } = 0.003;
    public TranscriptionSettings Transcription { get; set; } = new();
    public CleanupSettings Cleanup { get; set; } = new();

    public void Validate()
    {
        if (string.IsNullOrWhiteSpace(Hotkey))
            throw new ConfigurationException("hotkey must contain a key or shortcut, such as Ctrl+Alt+Space.");
        InRange(DoubleTapMs, 100, 1000, "doubleTapMs");
        InRange(TapMaxMs, 50, 1000, "tapMaxMs");
        InRange(MaxRecordingSeconds, 1, 300, "maxRecordingSeconds");
        InRange(ClipboardRestoreDelayMs, 100, 10000, "clipboardRestoreDelayMs");
        if (MicrophoneDevice < -1)
            throw new ConfigurationException("microphoneDevice must be -1 (Windows default) or a nonnegative device index.");
        if (!double.IsFinite(SilenceThreshold) || SilenceThreshold is < 0 or > 1)
            throw new ConfigurationException("silenceThreshold must be between 0 and 1.");
        if (Transcription is null || Cleanup is null)
            throw new ConfigurationException("transcription and cleanup must be JSON objects.");
        Transcription.Validate();
        Cleanup.Validate();
    }

    internal static void InRange(int value, int min, int max, string name)
    {
        if (value < min || value > max)
            throw new ConfigurationException($"{name} must be between {min} and {max}.");
    }

    internal static string Choice(string? value, string name, params string[] choices)
    {
        var normalized = value?.Trim().ToLowerInvariant();
        if (normalized is null || !choices.Contains(normalized))
            throw new ConfigurationException($"{name} must be one of: {string.Join(", ", choices)}.");
        return normalized;
    }
}

public sealed class TranscriptionSettings
{
    public string Provider { get; set; } = "local";
    public string LocalMode { get; set; } = "server";
    public string WhisperExecutable { get; set; } = "tools/whisper/whisper-cli.exe";
    public string WhisperServerExecutable { get; set; } = "tools/whisper/whisper-server.exe";
    public string ModelPath { get; set; } = "models/ggml-small.en.bin";
    public string Model { get; set; } = "";
    public string Language { get; set; } = "en";
    public int Threads { get; set; } = Math.Max(1, Environment.ProcessorCount / 2);
    public bool UseGpu { get; set; } = true;
    public int TimeoutSeconds { get; set; } = 120;
    public int StartupTimeoutSeconds { get; set; } = 120;

    internal void Validate()
    {
        Provider = AppSettings.Choice(Provider, "transcription.provider", "local", "groq", "openai");
        LocalMode = AppSettings.Choice(LocalMode, "transcription.localMode", "server", "cli");
        if (Provider == "local" && (string.IsNullOrWhiteSpace(WhisperExecutable) || string.IsNullOrWhiteSpace(ModelPath)))
            throw new ConfigurationException("Local transcription requires whisperExecutable and modelPath.");
        if (Provider == "local" && LocalMode == "server" && string.IsNullOrWhiteSpace(WhisperServerExecutable))
            throw new ConfigurationException("transcription.whisperServerExecutable must name the whisper.cpp server executable.");
        if (Language is null || Language.Length > 20 || Language.Any(c => !char.IsAsciiLetter(c) && c != '-'))
            throw new ConfigurationException("transcription.language must be a language code, auto, or an empty string.");
        Model ??= "";
        AppSettings.InRange(Threads, 1, 256, "transcription.threads");
        AppSettings.InRange(TimeoutSeconds, 1, 1800, "transcription.timeoutSeconds");
        AppSettings.InRange(StartupTimeoutSeconds, 1, 600, "transcription.startupTimeoutSeconds");
    }
}

public sealed class CleanupSettings
{
    // Auto stays offline with local transcription. Cloud transcription reuses its provider.
    public string Provider { get; set; } = "auto";
    public string Model { get; set; } = "";
    public string Endpoint { get; set; } = "http://localhost:11434/v1";
    public string Style { get; set; } = "natural";
    public int TimeoutSeconds { get; set; } = 30;
    public bool AutoStartLocal { get; set; }
    public string LocalExecutable { get; set; } = "tools/llama/llama-server.exe";
    public string LocalModelPath { get; set; } = "models/qwen2.5-3b-instruct-q4_k_m.gguf";
    public int GpuLayers { get; set; } = 99;

    internal void Validate()
    {
        Provider = AppSettings.Choice(Provider, "cleanup.provider", "auto", "none", "openai", "groq", "local");
        Style = AppSettings.Choice(Style, "cleanup.style", "natural", "sentence", "lowercase");
        Model ??= "";
        AppSettings.InRange(TimeoutSeconds, 1, 300, "cleanup.timeoutSeconds");
        AppSettings.InRange(GpuLayers, 0, 999, "cleanup.gpuLayers");
        if (Provider == "local")
        {
            if (!Uri.TryCreate(Endpoint, UriKind.Absolute, out var endpoint) ||
                endpoint.Scheme is not ("http" or "https") ||
                !endpoint.IsLoopback || !string.IsNullOrEmpty(endpoint.UserInfo) ||
                !string.IsNullOrEmpty(endpoint.Query) || !string.IsNullOrEmpty(endpoint.Fragment))
                throw new ConfigurationException("cleanup.endpoint must be an HTTP or HTTPS loopback URL, such as http://localhost:11434/v1.");
            if (string.IsNullOrWhiteSpace(Model) && !AutoStartLocal)
                throw new ConfigurationException("cleanup.model must name an installed model when cleanup.provider is local.");
            if (AutoStartLocal && (string.IsNullOrWhiteSpace(LocalExecutable) || string.IsNullOrWhiteSpace(LocalModelPath)))
                throw new ConfigurationException("Automatic local cleanup requires cleanup.localExecutable and cleanup.localModelPath.");
            if (AutoStartLocal && string.IsNullOrWhiteSpace(Model)) Model = "local";
        }
    }
}

public sealed class ConfigurationException(string message) : Exception(message);
