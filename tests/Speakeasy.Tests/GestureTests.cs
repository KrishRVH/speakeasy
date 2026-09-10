using Speakeasy.Core.Interaction;

namespace Speakeasy.Tests;

public sealed class GestureTests
{
    private static TimeSpan Ms(double value) => TimeSpan.FromMilliseconds(value);
    private static DictationGesture Create(double maxMs = 300_000) => new(Ms(300), Ms(220), Ms(maxMs));

    [Fact]
    public void HoldRecordsImmediatelyAndTranscribesOnRelease()
    {
        var gesture = Create();

        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(500)));
        Assert.Equal(DictationState.Held, gesture.State);
        Assert.Equal(Ms(500), gesture.RecordingStartedAt);
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyUp(Ms(1000)));
        Assert.Equal(DictationState.Processing, gesture.State);
        Assert.Equal(Ms(500), gesture.RecordingStartedAt);
        Assert.Empty(gesture.Complete());
        Assert.Equal(DictationState.Idle, gesture.State);
        Assert.Null(gesture.RecordingStartedAt);
    }

    [Fact]
    public void SingleQuickTapWaitsForDoubleTapDeadlineThenTranscribes()
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));

        Assert.Equal([GestureAction.ModeChanged], gesture.KeyUp(Ms(100)));
        Assert.Equal(DictationState.PendingTap, gesture.State);
        Assert.Empty(gesture.Tick(Ms(399)));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.Tick(Ms(400)));
        Assert.Equal(DictationState.Processing, gesture.State);
        Assert.Empty(gesture.Tick(Ms(500)));
    }

    [Theory]
    [InlineData(220, DictationState.PendingTap, GestureAction.ModeChanged)]
    [InlineData(221, DictationState.Processing, GestureAction.StopAndTranscribe)]
    public void TapDurationBoundaryIsInclusive(int releaseMs, DictationState state, GestureAction action)
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));

        Assert.Equal([action], gesture.KeyUp(Ms(releaseMs)));
        Assert.Equal(state, gesture.State);
    }

    [Fact]
    public void DoubleTapKeepsOriginalRecordingAndThirdDownStopsHandsFree()
    {
        var gesture = Create();
        gesture.KeyDown(Ms(100));
        gesture.KeyUp(Ms(200));

        Assert.Equal([GestureAction.ModeChanged], gesture.KeyDown(Ms(499)));
        Assert.Equal(DictationState.HandsFree, gesture.State);
        Assert.Equal(Ms(100), gesture.RecordingStartedAt);
        Assert.Empty(gesture.KeyUp(Ms(700)));
        Assert.Empty(gesture.Tick(Ms(5000)));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyDown(Ms(5100)));
        Assert.Equal(DictationState.Processing, gesture.State);
        Assert.Empty(gesture.KeyUp(Ms(5200)));
    }

    [Theory]
    [InlineData(400)]
    [InlineData(900)]
    public void LateSecondTapTriggersExpiredFirstClipInsteadOfStartingHandsFree(int secondDownMs)
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));
        gesture.KeyUp(Ms(100));

        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyDown(Ms(secondDownMs)));
        Assert.Equal(DictationState.Processing, gesture.State);
        Assert.Empty(gesture.KeyUp(Ms(secondDownMs + 10)));
        gesture.Complete();
        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(secondDownMs + 20)));
    }

    [Fact]
    public void RepeatedPhysicalDownAndUpEventsDoNotChangeGesture()
    {
        var gesture = Create();
        Assert.Empty(gesture.KeyUp(Ms(0)));
        gesture.KeyDown(Ms(100));
        Assert.Empty(gesture.KeyDown(Ms(200)));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyUp(Ms(400)));
        Assert.Empty(gesture.KeyUp(Ms(500)));

        gesture.Complete();
        gesture.KeyDown(Ms(600));
        gesture.KeyUp(Ms(650));
        Assert.Empty(gesture.KeyUp(Ms(700)));
        gesture.KeyDown(Ms(800));
        Assert.Empty(gesture.KeyDown(Ms(801)));
        Assert.Equal(DictationState.HandsFree, gesture.State);
        gesture.KeyUp(Ms(850));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyDown(Ms(900)));
    }

    [Theory]
    [InlineData(DictationState.Held)]
    [InlineData(DictationState.PendingTap)]
    [InlineData(DictationState.HandsFree)]
    [InlineData(DictationState.Processing)]
    public void EscapeCancelsEveryActiveStateAndIsIdempotent(DictationState target)
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));
        if (target == DictationState.Processing)
        {
            gesture.KeyUp(Ms(1000));
        }
        else if (target is DictationState.PendingTap or DictationState.HandsFree)
        {
            gesture.KeyUp(Ms(100));
            if (target == DictationState.HandsFree)
            {
                gesture.KeyDown(Ms(200));
            }
        }

        Assert.Equal(target, gesture.State);
        Assert.Equal([GestureAction.Cancel], gesture.Cancel());
        Assert.Equal(DictationState.Idle, gesture.State);
        Assert.Null(gesture.RecordingStartedAt);
        Assert.Empty(gesture.Cancel());
        Assert.Empty(gesture.Tick(Ms(300_000)));
    }

    [Fact]
    public void CancelWhileHeldRequiresReleaseBeforeAnotherRecording()
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));
        gesture.Cancel();

        Assert.Empty(gesture.KeyDown(Ms(10)));
        Assert.Empty(gesture.KeyUp(Ms(20)));
        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(30)));
    }

    [Fact]
    public void CompleteCannotDiscardAnActiveRecording()
    {
        var gesture = Create();
        Assert.Empty(gesture.Complete());
        gesture.KeyDown(Ms(0));
        Assert.Empty(gesture.Complete());
        Assert.Equal(DictationState.Held, gesture.State);
        gesture.KeyUp(Ms(100));
        gesture.Complete();
        Assert.Equal(DictationState.PendingTap, gesture.State);
        gesture.KeyDown(Ms(200));
        gesture.Complete();
        Assert.Equal(DictationState.HandsFree, gesture.State);
    }

    [Fact]
    public void HotkeysDuringProcessingAreIgnoredUntilPhysicalRelease()
    {
        var gesture = Create();
        gesture.KeyDown(Ms(0));
        gesture.KeyUp(Ms(1000));
        Assert.Empty(gesture.KeyDown(Ms(1100)));
        Assert.Empty(gesture.Tick(Ms(1200)));
        gesture.Complete();
        Assert.Empty(gesture.KeyDown(Ms(1300)));
        Assert.Equal(DictationState.Idle, gesture.State);
        Assert.Empty(gesture.KeyUp(Ms(1400)));
        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(1500)));
    }

    [Fact]
    public void AutoStopWhileHeldDoesNotRearmUntilReleaseAfterCompletion()
    {
        var gesture = Create(1000);
        gesture.KeyDown(Ms(0));
        Assert.Empty(gesture.Tick(Ms(999)));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.Tick(Ms(1000)));
        Assert.Empty(gesture.Tick(Ms(1001)));
        gesture.Complete();

        Assert.Empty(gesture.KeyDown(Ms(1010)));
        Assert.Empty(gesture.KeyUp(Ms(1020)));
        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(1030)));
    }

    [Fact]
    public void AutoStopOnReleaseEmitsOnlyOneTranscription()
    {
        var gesture = Create(1000);
        gesture.KeyDown(Ms(0));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyUp(Ms(1000)));
        Assert.Empty(gesture.Tick(Ms(1100)));
        gesture.Complete();
        Assert.Equal([GestureAction.StartRecording], gesture.KeyDown(Ms(1200)));
    }

    [Fact]
    public void PendingTapCannotExtendRecordingPastConfiguredLimit()
    {
        var gesture = Create(250);
        gesture.KeyDown(Ms(0));
        gesture.KeyUp(Ms(200));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.Tick(Ms(250)));
        Assert.Equal(DictationState.Processing, gesture.State);
    }

    [Fact]
    public void HandsFreeLimitStartsAtFirstDownAndAlwaysCapsAtFiveMinutes()
    {
        var gesture = Create(600_000);
        gesture.KeyDown(Ms(1000));
        gesture.KeyUp(Ms(1100));
        gesture.KeyDown(Ms(1200));
        gesture.KeyUp(Ms(1300));
        Assert.Empty(gesture.Tick(Ms(300_999)));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.Tick(Ms(301_000)));
        Assert.Equal(DictationState.Processing, gesture.State);
    }

    [Fact]
    public void RepeatedDownCanEnforceLimitWhenTimerHasNotRun()
    {
        var gesture = Create(1000);
        gesture.KeyDown(Ms(0));
        Assert.Equal([GestureAction.StopAndTranscribe], gesture.KeyDown(Ms(1000)));
        Assert.Empty(gesture.KeyDown(Ms(1100)));
    }

    [Theory]
    [InlineData(0, 220, 300_000)]
    [InlineData(-1, 220, 300_000)]
    [InlineData(300, 0, 300_000)]
    [InlineData(300, -1, 300_000)]
    [InlineData(300, 220, 0)]
    [InlineData(300, 220, -1)]
    public void NonPositiveDurationsAreRejected(int window, int tap, int limit)
    {
        Assert.Throws<ArgumentOutOfRangeException>(() => new DictationGesture(Ms(window), Ms(tap), Ms(limit)));
    }
}
