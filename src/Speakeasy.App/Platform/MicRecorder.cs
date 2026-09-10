using NAudio.Wave;
using Speakeasy.Core;

namespace Speakeasy.App.Platform;

/// <summary>
/// Opens the microphone only during a session. A wall-clock timer and a PCM byte limit
/// independently enforce five minutes, including when the UI thread is occupied.
/// Events are posted to the constructor's context, or to the pool when none is present.
/// </summary>
public sealed class MicRecorder : IAudioRecorder
{
    private readonly object _gate = new();
    private readonly int _deviceNumber;
    private readonly double _silenceThreshold;
    private readonly Func<IWaveIn>? _deviceFactory;
    private readonly SynchronizationContext? _context = SynchronizationContext.Current;
    private Session? _session;
    private bool _disposed;
    private float _level;

    public MicRecorder(int deviceNumber = -1, double silenceThreshold = 0.003)
    {
        if (deviceNumber < -1) throw new ArgumentOutOfRangeException(nameof(deviceNumber));
        if (!double.IsFinite(silenceThreshold) || silenceThreshold is < 0 or > 1)
            throw new ArgumentOutOfRangeException(nameof(silenceThreshold));
        _deviceNumber = deviceNumber;
        _silenceThreshold = silenceThreshold;
    }

    internal MicRecorder(Func<IWaveIn> deviceFactory, double silenceThreshold = 0.003)
        : this(-1, silenceThreshold) => _deviceFactory = deviceFactory;

    public event Action<Exception>? Failed;
    public event Action? LimitReached;
    public float Level => Volatile.Read(ref _level);

    public void Start()
    {
        Session session;
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (_session != null) throw new InvalidOperationException("A microphone session is already active.");
            // NAudio captures a context in its constructor. Use no context for its stop
            // callback so device cleanup never depends on the application's UI loop.
            var previousContext = SynchronizationContext.Current;
            IWaveIn device;
            try
            {
                SynchronizationContext.SetSynchronizationContext(null);
                device = _deviceFactory?.Invoke() ?? new WaveInEvent
                {
                    DeviceNumber = _deviceNumber,
                    WaveFormat = new WaveFormat(PcmRecording.SampleRate, 16, 1),
                    BufferMilliseconds = 40,
                    NumberOfBuffers = 3
                };
            }
            finally { SynchronizationContext.SetSynchronizationContext(previousContext); }

            session = new Session(device, new PcmRecording(_silenceThreshold));
            session.DataHandler = (_, e) => OnData(session, e);
            session.StoppedHandler = (_, e) => Finish(session, e.Exception);
            device.DataAvailable += session.DataHandler;
            device.RecordingStopped += session.StoppedHandler;
            _session = session;
            _level = 0;
            try
            {
                device.StartRecording();
                session.LimitTimer = new System.Threading.Timer(_ => ReachLimit(session), null,
                    TimeSpan.FromMinutes(5), Timeout.InfiniteTimeSpan);
            }
            catch
            {
                _session = null;
                session.Finished = true;
                session.Pcm.Dispose();
                DetachAndDispose(session);
                throw;
            }
        }
    }

    public async Task<RecordedAudio> StopAsync(CancellationToken cancellationToken)
    {
        Session session;
        lock (_gate)
            session = _session ?? throw new InvalidOperationException("There is no microphone session to stop.");
        using var registration = cancellationToken.Register(() => CancelSession(session));
        cancellationToken.ThrowIfCancellationRequested();
        RequestStop(session);
        try
        {
            var audio = await session.Completion.Task.WaitAsync(cancellationToken).ConfigureAwait(false);
            cancellationToken.ThrowIfCancellationRequested();
            return audio;
        }
        finally
        {
            lock (_gate)
                if (ReferenceEquals(_session, session)) _session = null;
        }
    }

    public void Cancel()
    {
        Session? session;
        lock (_gate) session = _session;
        if (session != null) CancelSession(session);
    }

    private void CancelSession(Session session)
    {
        lock (_gate)
        {
            session.Cancelled = true;
            if (ReferenceEquals(_session, session))
            {
                _session = null;
                Volatile.Write(ref _level, 0);
            }
            if (!session.Finished && !session.PcmDiscarded)
            {
                session.Pcm.Dispose();
                session.PcmDiscarded = true;
            }
            if (session.Finished && session.Completion.Task.IsCompletedSuccessfully)
                session.Completion.Task.Result.WaveData.AsSpan().Clear();
        }
        RequestStop(session);
    }

    private void OnData(Session session, WaveInEventArgs e)
    {
        bool atLimit;
        lock (_gate)
        {
            if (session.Cancelled || session.Finished) return;
            atLimit = session.Pcm.Append(e.Buffer.AsSpan(0, e.BytesRecorded));
            if (ReferenceEquals(_session, session)) Volatile.Write(ref _level, session.Pcm.Level);
        }
        if (atLimit) ReachLimit(session);
    }

    private void ReachLimit(Session session)
    {
        lock (_gate)
        {
            if (session.Cancelled || session.Finished || session.LimitNotified || session.StopRequested) return;
            session.LimitNotified = true;
        }
        RequestStop(session);
        Post(session, () => LimitReached?.Invoke());
    }

    private void RequestStop(Session session)
    {
        lock (_gate)
        {
            if (session.Finished || session.StopRequested) return;
            session.StopRequested = true;
            session.LimitTimer?.Dispose();
            session.LimitTimer = null;
        }
        // Repeat the stop request until acknowledged. This also covers WaveInEvent's
        // start/stop race when a tap ends before its capture worker has started.
        _ = StopUntilAcknowledgedAsync(session);
    }

    private async Task StopUntilAcknowledgedAsync(Session session)
    {
        for (var attempt = 0; attempt < 30; attempt++)
        {
            Exception? error = null;
            lock (_gate)
            {
                if (session.Finished) return;
                try { session.Device.StopRecording(); }
                catch (Exception ex) { error = ex; }
            }
            if (error != null) { Finish(session, error); return; }
            await Task.Delay(100).ConfigureAwait(false);
        }
        Finish(session, new IOException("The microphone did not acknowledge stop. Reconnect the device and try again."));
    }

    private void Finish(Session session, Exception? error)
    {
        RecordedAudio? result = null;
        bool cancelled;
        lock (_gate)
        {
            if (session.Finished) return;
            session.Finished = true;
            cancelled = session.Cancelled;
            if (!session.StopRequested && error == null)
                error = new IOException("The microphone stopped unexpectedly. Check the selected input device and Windows microphone permissions.");
            session.LimitTimer?.Dispose();
            session.LimitTimer = null;
            try
            {
                if (!cancelled && error == null) result = session.Pcm.Finish();
            }
            catch (Exception ex) { error = ex; }
            finally
            {
                if (!session.PcmDiscarded) session.Pcm.Dispose();
                session.PcmDiscarded = true;
            }
            if (ReferenceEquals(_session, session)) Volatile.Write(ref _level, 0);
        }

        try { DetachAndDispose(session); }
        catch (Exception ex) { error ??= ex; }
        lock (_gate) cancelled |= session.Cancelled;
        if (cancelled)
        {
            result?.WaveData.AsSpan().Clear();
            session.Completion.TrySetCanceled();
        }
        else if (error != null)
        {
            session.Completion.TrySetException(error);
            _ = session.Completion.Task.Exception; // Observe even when failure occurred before StopAsync.
            Post(session, () => Failed?.Invoke(error));
        }
        else session.Completion.TrySetResult(result!);
    }

    private static void DetachAndDispose(Session session)
    {
        session.Device.DataAvailable -= session.DataHandler;
        session.Device.RecordingStopped -= session.StoppedHandler;
        session.Device.Dispose();
    }

    private void Post(Session session, Action action)
    {
        void Deliver(object? _)
        {
            lock (_gate)
                if (_disposed || session.Cancelled || !ReferenceEquals(_session, session)) return;
            action();
        }
        try
        {
            if (_context != null) _context.Post(Deliver, null);
            else ThreadPool.QueueUserWorkItem(Deliver);
        }
        catch (System.ComponentModel.InvalidAsynchronousStateException) { }
        catch (ObjectDisposedException) { }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed) return;
            _disposed = true;
        }
        Cancel();
    }

    private sealed class Session(IWaveIn device, PcmRecording pcm)
    {
        internal IWaveIn Device { get; } = device;
        internal PcmRecording Pcm { get; } = pcm;
        internal TaskCompletionSource<RecordedAudio> Completion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal EventHandler<WaveInEventArgs>? DataHandler;
        internal EventHandler<StoppedEventArgs>? StoppedHandler;
        internal System.Threading.Timer? LimitTimer;
        internal bool StopRequested, Cancelled, Finished, LimitNotified, PcmDiscarded;
    }
}
