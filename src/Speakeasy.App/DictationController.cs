using System.Diagnostics;
using Speakeasy.Core;
using Speakeasy.Core.Configuration;
using Speakeasy.Core.Interaction;

namespace Speakeasy.App;

/// <summary>All transitions run on the UI thread. Each async operation owns a generation.</summary>
internal sealed class DictationController : IDisposable
{
    private readonly AppSettings _settings;
    private readonly DictationGesture _gesture;
    private readonly IAudioRecorder _recorder;
    private readonly ITranscriptionPipeline _pipeline;
    private readonly ITextInserter _inserter;
    private readonly SynchronizationContext _ui;
    private readonly System.Windows.Forms.Timer _timer = new() { Interval = 40 };
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    private CancellationTokenSource? _operation;
    private long _generation;
    private bool _disposed;

    public event Action? Changed;
    public event Action<string>? Notice;
    public DictationState State => _gesture.State;
    public string Status { get; private set; } = "Ready when you are";
    public string? LastNotice { get; private set; }
    public float Level => _recorder.Level;
    public TimeSpan Elapsed => _gesture.RecordingStartedAt is { } start ? _clock.Elapsed - start : TimeSpan.Zero;
    public bool IsEnabled { get; private set; }

    public DictationController(AppSettings settings, IAudioRecorder recorder,
        ITranscriptionPipeline pipeline, ITextInserter inserter)
    {
        _settings = settings;
        _recorder = recorder;
        _pipeline = pipeline;
        _inserter = inserter;
        _ui = SynchronizationContext.Current ?? throw new InvalidOperationException("A UI context is required.");
        _gesture = new(TimeSpan.FromMilliseconds(settings.DoubleTapMs),
            TimeSpan.FromMilliseconds(settings.TapMaxMs), TimeSpan.FromSeconds(settings.MaxRecordingSeconds));
        IsEnabled = settings.Enabled;
        _timer.Tick += (_, _) => { Apply(_gesture.Tick(_clock.Elapsed)); Changed?.Invoke(); };
        _recorder.Failed += OnRecorderFailed;
        _recorder.LimitReached += OnLimitReached;
    }

    public void KeyDown()
    {
        if (!_disposed && IsEnabled) Apply(_gesture.KeyDown(_clock.Elapsed));
    }

    public void ToggleHandsFree()
    {
        if (!_disposed && IsEnabled) Apply(_gesture.ToggleHandsFree(_clock.Elapsed));
    }

    public void KeyUp()
    {
        if (!_disposed) Apply(_gesture.KeyUp(_clock.Elapsed));
    }

    public void Cancel()
    {
        if (_disposed) return;
        Apply(_gesture.Cancel());
    }

    public void SetEnabled(bool enabled)
    {
        Cancel();
        IsEnabled = enabled;
        if (enabled) LastNotice = null;
        Status = enabled ? "Ready when you are" : "Dictation is paused";
        Changed?.Invoke();
    }

    private void Apply(IReadOnlyList<GestureAction> actions)
    {
        if (_disposed) return;
        foreach (var action in actions)
        {
            switch (action)
            {
                case GestureAction.StartRecording:
                    try
                    {
                        _generation++;
                        LastNotice = null;
                        _recorder.Start();
                        Status = State == DictationState.HandsFree ? "Listening hands-free" : "Listening";
                        _timer.Start();
                    }
                    catch (Exception exception)
                    {
                        Apply(_gesture.Cancel());
                        Status = "Microphone unavailable";
                        ReportNotice(exception.Message);
                    }
                    break;
                case GestureAction.ModeChanged:
                    Status = State == DictationState.HandsFree ? "Listening hands-free" : "Listening";
                    break;
                case GestureAction.StopAndTranscribe:
                    _operation = new CancellationTokenSource();
                    _ = FinishAsync(_generation, _operation);
                    break;
                case GestureAction.Cancel:
                    ++_generation;
                    _operation?.Cancel();
                    _recorder.Cancel();
                    _timer.Stop();
                    Status = "Cancelled";
                    break;
            }
        }
        Changed?.Invoke();
    }

    private async Task FinishAsync(long generation, CancellationTokenSource operation)
    {
        var token = operation.Token;
        bool IsCurrent() => !_disposed && generation == _generation && !token.IsCancellationRequested
            && ReferenceEquals(_operation, operation);
        try
        {
            Status = "Finishing audio";
            var audio = await _recorder.StopAsync(token);
            if (!IsCurrent()) return;
            if (!audio.HasSpeech || audio.Duration.TotalMilliseconds < 200)
            {
                Status = "No speech detected";
                return;
            }
            var transcript = await _pipeline.TranscribeAsync(audio, stage => _ui.Post(_ =>
            {
                if (!IsCurrent()) return;
                Status = stage;
                Changed?.Invoke();
            }, null), token);
            if (!IsCurrent()) return;
            if (string.IsNullOrWhiteSpace(transcript.Text))
            {
                Status = "No speech detected";
                return;
            }
            Status = "Pasting";
            Changed?.Invoke();
            var result = await _inserter.InsertAsync(transcript.Text, _settings.RestoreClipboard,
                _settings.ClipboardRestoreDelayMs, token);
            if (!IsCurrent()) return;
            Status = result.Pasted ? "Ready when you are" : "Automatic paste unavailable";
            if (transcript.Warning is { } warning) ReportNotice(warning);
            if (result.Message is { } message) ReportNotice(message);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception exception)
        {
            if (IsCurrent())
            {
                Status = "Could not finish dictation";
                ReportNotice(exception.Message);
            }
        }
        finally
        {
            if (!_disposed && generation == _generation)
            {
                _gesture.Complete();
                _timer.Stop();
                Changed?.Invoke();
            }
            if (ReferenceEquals(_operation, operation)) _operation = null;
            operation.Dispose();
        }
    }

    private void OnRecorderFailed(Exception exception)
    {
        var generation = Volatile.Read(ref _generation);
        _ui.Post(_ =>
        {
            if (_disposed || generation != _generation || State is DictationState.Idle or DictationState.Processing) return;
            Cancel();
            Status = "Microphone disconnected";
            ReportNotice(exception.Message);
            Changed?.Invoke();
        }, null);
    }

    private void OnLimitReached()
    {
        var generation = Volatile.Read(ref _generation);
        _ui.Post(_ =>
        {
            if (_disposed || generation != _generation) return;
            // The recorder independently caps captured samples even if the UI was briefly busy.
            Apply(_gesture.Tick((_gesture.RecordingStartedAt ?? _clock.Elapsed)
                + TimeSpan.FromSeconds(_settings.MaxRecordingSeconds)));
        }, null);
    }

    private void ReportNotice(string message)
    {
        LastNotice = message;
        Notice?.Invoke(message);
    }

    public void Dispose()
    {
        if (_disposed) return;
        Cancel();
        _disposed = true;
        _timer.Dispose();
        _recorder.Failed -= OnRecorderFailed;
        _recorder.LimitReached -= OnLimitReached;
        _recorder.Dispose();
        _pipeline.Dispose();
        if (_inserter is IDisposable disposable) disposable.Dispose();
    }
}
