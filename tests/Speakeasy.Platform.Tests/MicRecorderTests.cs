using NAudio.Wave;
using Speakeasy.App.Platform;

namespace Speakeasy.Platform.Tests;

public sealed class MicRecorderTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task SlowDriverStopDoesNotBlockStopOrCancelCaller(bool cancelPendingStop)
    {
        using var releaseDriver = new ManualResetEventSlim();
        var enteredDriver = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var stopReturned = new TaskCompletionSource<Task>(TaskCreationOptions.RunContinuationsAsynchronously);
        var device = new FakeInput
        {
            OnStop = () =>
            {
                enteredDriver.TrySetResult();
                if (!releaseDriver.Wait(TimeSpan.FromSeconds(5))) throw new TimeoutException("Fake driver was not released.");
            }
        };
        using var recorder = new MicRecorder(() => device);
        recorder.Start();
        var beginStop = Task.Run(() => stopReturned.TrySetResult(recorder.StopAsync(CancellationToken.None)));
        Task? cancel = null;
        try
        {
            await enteredDriver.Task.WaitAsync(TimeSpan.FromSeconds(2));
            var responsiveCall = cancelPendingStop ? cancel = Task.Run(recorder.Cancel) : stopReturned.Task;
            Assert.Same(responsiveCall, await Task.WhenAny(responsiveCall, Task.Delay(500)));
        }
        finally
        {
            releaseDriver.Set();
            await beginStop.WaitAsync(TimeSpan.FromSeconds(2));
            if (cancel != null) await cancel.WaitAsync(TimeSpan.FromSeconds(2));
            var completion = await stopReturned.Task.WaitAsync(TimeSpan.FromSeconds(2));
            try { await completion.WaitAsync(TimeSpan.FromSeconds(2)); }
            catch (OperationCanceledException) when (cancelPendingStop) { }
        }
    }

    [Fact]
    public async Task StopFinalizesAudioAndReleasesDeviceBeforeCompleting()
    {
        var device = new FakeInput();
        using var recorder = new MicRecorder(() => device);
        recorder.Start();
        device.Emit(new byte[3200]);
        var audio = await recorder.StopAsync(CancellationToken.None);
        Assert.Equal(TimeSpan.FromMilliseconds(100), audio.Duration);
        Assert.Equal(3244, audio.WaveData.Length);
        Assert.True(device.Disposed);
        Assert.Equal(0, recorder.Level);
    }

    [Fact]
    public async Task AStoppedSessionDoesNotPreventStartingAnother()
    {
        var devices = new List<FakeInput>();
        using var recorder = new MicRecorder(() =>
        {
            var device = new FakeInput();
            devices.Add(device);
            return device;
        });
        recorder.Start();
        await recorder.StopAsync(CancellationToken.None);
        recorder.Start();
        await recorder.StopAsync(CancellationToken.None);
        Assert.Equal(2, devices.Count);
        Assert.All(devices, input => Assert.True(input.Disposed));
    }

    [Fact]
    public async Task CancellationDropsPendingAudioAndDoesNotStopNewSession()
    {
        var first = new FakeInput { StopImmediately = false };
        var second = new FakeInput();
        var next = first;
        using var recorder = new MicRecorder(() => next);
        using var cancellation = new CancellationTokenSource();
        recorder.Start();
        first.Emit(new byte[3200]);
        var stopping = recorder.StopAsync(cancellation.Token);
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => stopping);
        next = second;
        recorder.Start();
        first.Stopped();
        Assert.False(second.Disposed);
        second.Emit(new byte[6400]);
        var audio = await recorder.StopAsync(CancellationToken.None);
        Assert.Equal(TimeSpan.FromMilliseconds(200), audio.Duration);
    }

    [Fact]
    public async Task SampleCapStopsDeviceWithoutNeedingUiMessagePump()
    {
        var device = new FakeInput();
        using var recorder = new MicRecorder(() => device);
        recorder.Start();
        device.Emit(new byte[PcmRecording.MaximumBytes + 100]);
        await device.DisposedCompletion.Task.WaitAsync(TimeSpan.FromSeconds(2));
        Assert.True(device.Disposed);
        var audio = await recorder.StopAsync(CancellationToken.None);
        Assert.Equal(TimeSpan.FromMinutes(5), audio.Duration);
    }

    [Fact]
    public async Task QueuedFailureFromCancelledSessionCannotAffectNewRecording()
    {
        var context = new QueuedContext();
        var previous = SynchronizationContext.Current;
        var first = new FakeInput();
        var second = new FakeInput();
        var next = first;
        MicRecorder recorder;
        try
        {
            SynchronizationContext.SetSynchronizationContext(context);
            recorder = new MicRecorder(() => next);
        }
        finally { SynchronizationContext.SetSynchronizationContext(previous); }
        using (recorder)
        {
            var failures = 0;
            recorder.Failed += _ => failures++;
            recorder.Start();
            first.Stopped(new IOException("Disconnected"));
            recorder.Cancel();
            next = second;
            recorder.Start();
            context.Drain();
            Assert.Equal(0, failures);
            Assert.False(second.Disposed);
            await recorder.StopAsync(CancellationToken.None);
        }
    }

    private sealed class QueuedContext : SynchronizationContext
    {
        private readonly Queue<Action> _callbacks = new();
        public override void Post(SendOrPostCallback d, object? state) => _callbacks.Enqueue(() => d(state));
        internal void Drain() { while (_callbacks.TryDequeue(out var callback)) callback(); }
    }

    private sealed class FakeInput : IWaveIn
    {
        public WaveFormat WaveFormat { get; set; } = new(16_000, 16, 1);
        public event EventHandler<WaveInEventArgs>? DataAvailable;
        public event EventHandler<StoppedEventArgs>? RecordingStopped;
        internal bool StopImmediately { get; init; } = true;
        internal Action? OnStop { get; init; }
        internal bool Disposed { get; private set; }
        internal TaskCompletionSource DisposedCompletion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public void StartRecording() { }
        public void StopRecording() { OnStop?.Invoke(); if (StopImmediately) Stopped(); }
        public void Dispose() { Disposed = true; DisposedCompletion.TrySetResult(); }
        internal void Emit(byte[] data) => DataAvailable?.Invoke(this, new WaveInEventArgs(data, data.Length));
        internal void Stopped(Exception? error = null) => RecordingStopped?.Invoke(this, new StoppedEventArgs(error));
    }
}
