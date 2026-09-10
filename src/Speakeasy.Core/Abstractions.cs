namespace Speakeasy.Core;

public sealed record RecordedAudio(byte[] WaveData, TimeSpan Duration, bool HasSpeech);
public sealed record TranscriptionResult(string Text, string? Warning = null);
public sealed record InsertionResult(bool Pasted, string? Message = null);

public interface IAudioRecorder : IDisposable
{
    event Action<Exception>? Failed;
    event Action? LimitReached;
    float Level { get; }
    void Start();
    Task<RecordedAudio> StopAsync(CancellationToken cancellationToken);
    void Cancel();
}

public interface ITranscriptionPipeline : IDisposable
{
    Task<TranscriptionResult> TranscribeAsync(RecordedAudio audio, Action<string>? stage, CancellationToken cancellationToken);
}

public interface ITextInserter
{
    Task<InsertionResult> InsertAsync(string text, bool restoreClipboard, int restoreDelayMs, CancellationToken cancellationToken);
}
