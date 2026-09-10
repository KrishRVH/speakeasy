using System.Collections.Concurrent;
using System.Runtime.ExceptionServices;
using Speakeasy.App;
using Speakeasy.Core;
using Speakeasy.Core.Configuration;
using Speakeasy.Core.Interaction;

namespace Speakeasy.Platform.Tests;

public sealed class ControllerTests
{
    [Fact]
    public void EscapeDuringCaptureDropsAudioAndRequiresPhysicalRelease() => Run(fixture =>
    {
        fixture.Controller.KeyDown();
        fixture.Controller.Cancel();

        Assert.Equal(DictationState.Idle, fixture.Controller.State);
        Assert.Equal(1, fixture.Recorder.CancelCount);
        Assert.Empty(fixture.Recorder.Stops);
        Assert.Empty(fixture.Pipeline.Calls);
        fixture.Controller.KeyDown();
        Assert.Equal(1, fixture.Recorder.StartCount);
        fixture.Controller.KeyUp();
        fixture.Controller.KeyDown();
        Assert.Equal(2, fixture.Recorder.StartCount);
    });

    [Fact]
    public void EscapeWhileAudioIsFinishingRejectsEvenAnUncooperativeLateResult() => Run(fixture =>
    {
        var stop = fixture.BeginProcessing();
        fixture.Controller.Cancel();

        Assert.True(stop.Token.IsCancellationRequested);
        stop.Complete(Speech());
        fixture.Context.Drain();

        Assert.Equal(DictationState.Idle, fixture.Controller.State);
        Assert.Equal("Cancelled", fixture.Controller.Status);
        Assert.Empty(fixture.Pipeline.Calls);
        Assert.Empty(fixture.Inserter.Calls);
    });

    [Theory]
    [InlineData("Transcribing")]
    [InlineData("Cleaning up")]
    public void EscapeDuringTranscriptionOrCleanupPreventsInsertion(string stage) => Run(fixture =>
    {
        var call = fixture.ReachPipeline();
        call.Report(stage);
        fixture.Context.Drain();
        Assert.Equal(stage, fixture.Controller.Status);

        fixture.Controller.Cancel();
        Assert.True(call.Token.IsCancellationRequested);
        call.Complete("This result arrived after cancellation.");
        fixture.Context.Drain();

        Assert.Equal(DictationState.Idle, fixture.Controller.State);
        Assert.Equal("Cancelled", fixture.Controller.Status);
        Assert.Empty(fixture.Inserter.Calls);
    });

    [Fact]
    public void EscapeDuringInsertionCancelsTheInsertionTokenBeforeCommit() => Run(fixture =>
    {
        var insertion = fixture.ReachInsertion();
        Assert.Equal("Pasting", fixture.Controller.Status);
        fixture.Controller.Cancel();

        Assert.True(insertion.Token.IsCancellationRequested);
        insertion.AttemptCommit();
        fixture.Context.Drain();

        Assert.False(insertion.Pasted);
        Assert.Equal(DictationState.Idle, fixture.Controller.State);
        Assert.Equal("Cancelled", fixture.Controller.Status);
    });

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void DisablingCancelsCaptureOrProcessingAndIgnoresNewHotkeys(bool processing) => Run(fixture =>
    {
        PipelineCall? call = null;
        if (processing) call = fixture.ReachPipeline();
        else fixture.Controller.KeyDown();

        fixture.Controller.SetEnabled(false);
        fixture.Controller.KeyUp();
        fixture.Controller.KeyDown();
        call?.Complete("Must not be pasted.");
        fixture.Context.Drain();

        Assert.False(fixture.Controller.IsEnabled);
        Assert.Equal(1, fixture.Recorder.StartCount);
        Assert.Equal(DictationState.Idle, fixture.Controller.State);
        Assert.Equal("Dictation is paused", fixture.Controller.Status);
        Assert.Empty(fixture.Inserter.Calls);
        if (call is not null) Assert.True(call.Token.IsCancellationRequested);
    });

    [Fact]
    public void OldPipelineCompletionCannotFinishOrPasteIntoNewRecording() => Run(fixture =>
    {
        var oldCall = fixture.ReachPipeline();
        fixture.Controller.Cancel();
        fixture.Controller.KeyDown();

        oldCall.Report("Old cleanup stage");
        oldCall.Complete("Old text");
        fixture.Context.Drain();

        Assert.Equal(DictationState.Held, fixture.Controller.State);
        Assert.Equal("Listening", fixture.Controller.Status);
        Assert.Empty(fixture.Inserter.Calls);
        fixture.Recorder.RaiseLimit();
        fixture.Context.Drain();
        fixture.Controller.KeyUp();
        fixture.Recorder.Stops[^1].Complete(Speech());
        fixture.Pipeline.Calls[^1].Complete("New text");
        fixture.Inserter.Calls[^1].AttemptCommit();

        var insertion = Assert.Single(fixture.Inserter.Calls);
        Assert.Equal("New text", insertion.Text);
        Assert.True(insertion.Pasted);
        Assert.Equal(DictationState.Idle, fixture.Controller.State);
    });

    [Fact]
    public void OldAudioCompletionCannotCompleteNewProcessingGeneration() => Run(fixture =>
    {
        var oldStop = fixture.BeginProcessing();
        fixture.Controller.Cancel();
        var newStop = fixture.BeginProcessing();

        oldStop.Complete(Speech());
        fixture.Context.Drain();
        Assert.Equal(DictationState.Processing, fixture.Controller.State);
        Assert.Empty(fixture.Pipeline.Calls);
        Assert.False(newStop.Token.IsCancellationRequested);

        newStop.Complete(Speech());
        Assert.Single(fixture.Pipeline.Calls).Complete("Only current text");
        Assert.Single(fixture.Inserter.Calls).AttemptCommit();
        Assert.Equal(DictationState.Idle, fixture.Controller.State);
    });

    [Fact]
    public void OldPipelineFailureDoesNotNotifyOrDisturbNewSession() => Run(fixture =>
    {
        var oldCall = fixture.ReachPipeline();
        fixture.Controller.Cancel();
        fixture.Controller.KeyDown();

        oldCall.Fail(new IOException("Old request failed."));
        fixture.Context.Drain();

        Assert.Equal(DictationState.Held, fixture.Controller.State);
        Assert.Equal("Listening", fixture.Controller.Status);
        Assert.Empty(fixture.Notices);
    });

    [Fact]
    public void QueuedRecorderFailureCannotCancelTheNextRecording() => Run(fixture =>
    {
        fixture.Controller.KeyDown();
        fixture.Recorder.RaiseFailure();
        fixture.Controller.Cancel();
        fixture.Controller.KeyUp();
        fixture.Controller.KeyDown();
        fixture.Context.Drain();

        Assert.Equal(DictationState.Held, fixture.Controller.State);
        Assert.Equal("Listening", fixture.Controller.Status);
        Assert.Empty(fixture.Notices);
    });

    [Fact]
    public void QueuedRecordingLimitCannotStopTheNextRecording() => Run(fixture =>
    {
        fixture.Controller.KeyDown();
        fixture.Recorder.RaiseLimit();
        fixture.Controller.Cancel();
        fixture.Controller.KeyUp();
        fixture.Controller.KeyDown();
        fixture.Context.Drain();

        Assert.Equal(DictationState.Held, fixture.Controller.State);
        Assert.Empty(fixture.Recorder.Stops);
        Assert.Empty(fixture.Pipeline.Calls);
    });

    [Fact]
    public void QueuedStageCannotOverwriteSuccessfulCompletionStatus() => Run(fixture =>
    {
        var call = fixture.ReachPipeline();
        call.Report("Cleaning up");
        call.Complete("Finished text");
        Assert.Single(fixture.Inserter.Calls).AttemptCommit();
        Assert.Equal(DictationState.Idle, fixture.Controller.State);

        fixture.Context.Drain();

        Assert.Equal("Ready when you are", fixture.Controller.Status);
    });

    [Fact]
    public void EmptyOrTooShortCaptureDoesNotCallTranscription() => Run(fixture =>
    {
        fixture.BeginProcessing().Complete(new RecordedAudio([], TimeSpan.FromSeconds(1), false));
        Assert.Empty(fixture.Pipeline.Calls);
        Assert.Equal("No speech detected", fixture.Controller.Status);
        Assert.Equal(DictationState.Idle, fixture.Controller.State);

        fixture.BeginProcessing().Complete(new RecordedAudio([1], TimeSpan.FromMilliseconds(100), true));
        Assert.Empty(fixture.Pipeline.Calls);
        Assert.Equal(DictationState.Idle, fixture.Controller.State);
    });

    [Fact]
    public void DisposeCancelsAnOutstandingRequestAndSuppressesItsCallbacks() => Run(fixture =>
    {
        var call = fixture.ReachPipeline();
        fixture.Controller.Dispose();

        Assert.True(call.Token.IsCancellationRequested);
        Assert.True(fixture.Recorder.Disposed);
        Assert.True(fixture.Pipeline.Disposed);
        call.Report("Late stage");
        call.Complete("Late text");
        fixture.Context.Drain();
        Assert.Empty(fixture.Inserter.Calls);
        Assert.Empty(fixture.Notices);
    });

    private static RecordedAudio Speech() => new([1, 2, 3], TimeSpan.FromSeconds(1), true);

    // The controller requires a UI context and creates a WinForms timer. Run each
    // test on its own STA thread with a deterministic queue; no microphone,
    // clipboard, global hook, real windows, or native input is touched.
    private static void Run(Action<Fixture> test)
    {
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            var context = new QueuedContext();
            SynchronizationContext.SetSynchronizationContext(context);
            try
            {
                using var fixture = new Fixture(context);
                test(fixture);
            }
            catch (Exception exception)
            {
                failure = exception;
            }
            finally
            {
                SynchronizationContext.SetSynchronizationContext(null);
            }
        })
        { IsBackground = true };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        Assert.True(thread.Join(TimeSpan.FromSeconds(10)), "Controller test exceeded its timeout.");
        if (failure is not null) ExceptionDispatchInfo.Capture(failure).Throw();
    }

    private sealed class QueuedContext : SynchronizationContext
    {
        private readonly ConcurrentQueue<(SendOrPostCallback Callback, object? State)> _queue = new();

        public override void Post(SendOrPostCallback d, object? state) => _queue.Enqueue((d, state));

        public void Drain()
        {
            while (_queue.TryDequeue(out var work)) work.Callback(work.State);
        }
    }

    private sealed class Fixture : IDisposable
    {
        public QueuedContext Context { get; }
        public FakeRecorder Recorder { get; } = new();
        public FakePipeline Pipeline { get; } = new();
        public FakeInserter Inserter { get; } = new();
        public DictationController Controller { get; }
        public List<string> Notices { get; } = [];

        public Fixture(QueuedContext context)
        {
            Context = context;
            Controller = new(new AppSettings(), Recorder, Pipeline, Inserter);
            Controller.Notice += Notices.Add;
        }

        public StopCall BeginProcessing()
        {
            Controller.KeyDown();
            Recorder.RaiseLimit();
            Context.Drain();
            Controller.KeyUp();
            Assert.Equal(DictationState.Processing, Controller.State);
            return Recorder.Stops[^1];
        }

        public PipelineCall ReachPipeline()
        {
            BeginProcessing().Complete(Speech());
            return Pipeline.Calls[^1];
        }

        public InsertCall ReachInsertion()
        {
            ReachPipeline().Complete("A dictated sentence.");
            return Inserter.Calls[^1];
        }

        public void Dispose() => Controller.Dispose();
    }

    private sealed class FakeRecorder : IAudioRecorder
    {
        public event Action<Exception>? Failed;
        public event Action? LimitReached;
        public float Level => 0;
        public int StartCount { get; private set; }
        public int CancelCount { get; private set; }
        public bool Disposed { get; private set; }
        public List<StopCall> Stops { get; } = [];
        public void Start() => StartCount++;
        public void Cancel() => CancelCount++;
        public void Dispose() => Disposed = true;
        public void RaiseLimit() => LimitReached?.Invoke();
        public void RaiseFailure() => Failed?.Invoke(new IOException("The microphone was disconnected."));

        public Task<RecordedAudio> StopAsync(CancellationToken cancellationToken)
        {
            var call = new StopCall(cancellationToken);
            Stops.Add(call);
            return call.Source.Task;
        }
    }

    private sealed class StopCall(CancellationToken token)
    {
        public CancellationToken Token { get; } = token;
        public TaskCompletionSource<RecordedAudio> Source { get; } = new();
        // Deliberately returns late results even after cancellation to exercise
        // the controller's generation check independently of provider behavior.
        public void Complete(RecordedAudio audio) => Source.SetResult(audio);
    }

    private sealed class FakePipeline : ITranscriptionPipeline
    {
        public List<PipelineCall> Calls { get; } = [];
        public bool Disposed { get; private set; }
        public void Dispose() => Disposed = true;

        public Task<TranscriptionResult> TranscribeAsync(RecordedAudio audio,
            Action<string>? stage, CancellationToken cancellationToken)
        {
            var call = new PipelineCall(stage, cancellationToken);
            Calls.Add(call);
            return call.Source.Task;
        }
    }

    private sealed class PipelineCall(Action<string>? stage, CancellationToken token)
    {
        public CancellationToken Token { get; } = token;
        public TaskCompletionSource<TranscriptionResult> Source { get; } = new();
        public void Report(string value) => stage?.Invoke(value);
        public void Complete(string text) => Source.SetResult(new(text));
        public void Fail(Exception exception) => Source.SetException(exception);
    }

    private sealed class FakeInserter : ITextInserter
    {
        public List<InsertCall> Calls { get; } = [];

        public Task<InsertionResult> InsertAsync(string text, bool restoreClipboard,
            int restoreDelayMs, CancellationToken cancellationToken)
        {
            var call = new InsertCall(text, cancellationToken);
            Calls.Add(call);
            return call.Source.Task;
        }
    }

    private sealed class InsertCall(string text, CancellationToken token)
    {
        public string Text { get; } = text;
        public CancellationToken Token { get; } = token;
        public bool Pasted { get; private set; }
        public TaskCompletionSource<InsertionResult> Source { get; } = new();

        public void AttemptCommit()
        {
            if (Token.IsCancellationRequested)
            {
                Source.SetCanceled(Token);
                return;
            }

            Pasted = true;
            Source.SetResult(new(true));
        }
    }
}
