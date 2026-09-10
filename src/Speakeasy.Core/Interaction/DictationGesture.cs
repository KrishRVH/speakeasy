namespace Speakeasy.Core.Interaction;

public enum DictationState
{
    Idle,
    Held,
    PendingTap,
    HandsFree,
    Processing
}

public enum GestureAction
{
    StartRecording,
    StopAndTranscribe,
    Cancel,
    ModeChanged
}

/// <summary>
/// Converts physical hotkey edges into dictation actions. Call from one thread,
/// use a monotonic clock for <c>now</c>, and call <see cref="Tick"/> periodically.
/// Audio and transcription are owned by the caller.
/// </summary>
public sealed class DictationGesture
{
    private static readonly IReadOnlyList<GestureAction> NoActions = Array.Empty<GestureAction>();
    private static readonly IReadOnlyList<GestureAction> StartAction = Array.AsReadOnly(new[] { GestureAction.StartRecording });
    private static readonly IReadOnlyList<GestureAction> StopAction = Array.AsReadOnly(new[] { GestureAction.StopAndTranscribe });
    private static readonly IReadOnlyList<GestureAction> CancelAction = Array.AsReadOnly(new[] { GestureAction.Cancel });
    private static readonly IReadOnlyList<GestureAction> ModeAction = Array.AsReadOnly(new[] { GestureAction.ModeChanged });
    private static readonly TimeSpan HardRecordingLimit = TimeSpan.FromMinutes(5);

    private readonly TimeSpan _doubleTapWindow;
    private readonly TimeSpan _tapMaxDuration;
    private readonly TimeSpan _maxRecordingDuration;
    private bool _keyIsDown;
    private TimeSpan _keyDownAt;
    private TimeSpan _firstReleaseAt;

    public DictationGesture(TimeSpan doubleTapWindow, TimeSpan tapMaxDuration, TimeSpan maxRecordingDuration)
    {
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(doubleTapWindow, TimeSpan.Zero);
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(tapMaxDuration, TimeSpan.Zero);
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(maxRecordingDuration, TimeSpan.Zero);

        _doubleTapWindow = doubleTapWindow;
        _tapMaxDuration = tapMaxDuration;
        _maxRecordingDuration = maxRecordingDuration < HardRecordingLimit ? maxRecordingDuration : HardRecordingLimit;
    }

    public DictationState State { get; private set; } = DictationState.Idle;

    /// <summary>The original recording start, retained through processing until completion or cancellation.</summary>
    public TimeSpan? RecordingStartedAt { get; private set; }

    /// <summary>Starts or finishes recording from the UI without changing physical key state.</summary>
    public IReadOnlyList<GestureAction> ToggleHandsFree(TimeSpan now)
    {
        if (State == DictationState.Idle)
        {
            RecordingStartedAt = now;
            State = DictationState.HandsFree;
            return StartAction;
        }

        return State is DictationState.Held or DictationState.PendingTap or DictationState.HandsFree
            ? Stop() : NoActions;
    }

    public IReadOnlyList<GestureAction> KeyDown(TimeSpan now)
    {
        // Expiration wins over a late second tap, even when a timer tick was delayed.
        var timedActions = Tick(now);
        if (_keyIsDown)
        {
            return timedActions;
        }

        _keyIsDown = true;
        _keyDownAt = now;
        if (timedActions.Count > 0)
        {
            return timedActions;
        }

        switch (State)
        {
            case DictationState.Idle:
                RecordingStartedAt = now;
                State = DictationState.Held;
                return StartAction;
            case DictationState.PendingTap:
                State = DictationState.HandsFree;
                return ModeAction;
            case DictationState.HandsFree:
                return Stop();
            default:
                return NoActions;
        }
    }

    public IReadOnlyList<GestureAction> KeyUp(TimeSpan now)
    {
        var timedActions = Tick(now);
        if (!_keyIsDown)
        {
            return timedActions;
        }

        _keyIsDown = false;
        if (timedActions.Count > 0 || State != DictationState.Held)
        {
            return timedActions;
        }

        if (now - _keyDownAt <= _tapMaxDuration)
        {
            // Keep recording during this interval so a hands-free double tap
            // preserves the first words. A second down at the deadline is late.
            _firstReleaseAt = now;
            State = DictationState.PendingTap;
            return ModeAction;
        }

        return Stop();
    }

    public IReadOnlyList<GestureAction> Tick(TimeSpan now)
    {
        if (State is DictationState.Held or DictationState.PendingTap or DictationState.HandsFree)
        {
            if (RecordingStartedAt is { } started && now - started >= _maxRecordingDuration)
            {
                return Stop();
            }

            if (State == DictationState.PendingTap && now - _firstReleaseAt >= _doubleTapWindow)
            {
                return Stop();
            }
        }

        return NoActions;
    }

    public IReadOnlyList<GestureAction> Cancel()
    {
        if (State == DictationState.Idle)
        {
            return NoActions;
        }

        Reset();
        return CancelAction;
    }

    public IReadOnlyList<GestureAction> Complete()
    {
        if (State == DictationState.Processing)
        {
            Reset();
        }

        return NoActions;
    }

    private IReadOnlyList<GestureAction> Stop()
    {
        State = DictationState.Processing;
        return StopAction;
    }

    private void Reset()
    {
        State = DictationState.Idle;
        RecordingStartedAt = null;
        // Physical key state deliberately survives cancellation/completion:
        // a still-held key must be released before another recording can start.
    }
}
