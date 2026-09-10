using System.Diagnostics;
using Speakeasy.App.Platform;

namespace Speakeasy.App.UI;

/// <summary>A short, local input check. Captured audio is always discarded.</summary>
internal sealed class MicrophoneCheckForm : Form
{
    private readonly MicRecorder _recorder;
    private readonly System.Windows.Forms.Timer _timer = new() { Interval = 50 };
    private readonly Stopwatch _clock = new();
    private readonly Label _status;
    private readonly ProgressBar _level;
    private readonly SoftButton _check;
    private bool _heardAudio;
    private bool _disposed;

    public MicrophoneCheckForm(int device, string name, double silenceThreshold)
    {
        SuspendLayout();
        Text = "Check microphone · speakeasy";
        ClientSize = new Size(520, 316);
        BackColor = Theme.Background;
        ForeColor = Theme.Ink;
        Font = Theme.Font(10);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        ShowInTaskbar = true;
        StartPosition = FormStartPosition.CenterParent;
        Icon = Theme.MakeIcon();
        Controls.Add(new Label { Text = "Can we hear you?", Location = new Point(24, 20), Size = new Size(472, 36), Font = Theme.Font(21, FontStyle.Bold) });
        Controls.Add(new Label { Text = name, Location = new Point(26, 67), Size = new Size(468, 25), AutoEllipsis = true, ForeColor = Theme.Muted });
        _level = new ProgressBar { Location = new Point(26, 110), Size = new Size(468, 18), Maximum = 100, AccessibleName = "Microphone input level" };
        _status = new Label { Location = new Point(26, 141), Size = new Size(468, 42), ForeColor = Theme.Green };
        Controls.Add(new Label { Text = "Speak for a moment. This check lasts 10 seconds.\nAudio stays here and is discarded; nothing is transcribed.", Location = new Point(26, 191), Size = new Size(468, 40), ForeColor = Theme.Muted, Font = Theme.Font(9) });
        _check = new SoftButton { Text = "Check again", Location = new Point(26, 252), Size = new Size(154, 40), Primary = true };
        _check.Click += (_, _) => StartCheck();
        var close = new SoftButton { Text = "Done", Location = new Point(386, 252), Size = new Size(108, 40), DialogResult = DialogResult.Cancel };
        close.Click += (_, _) => Close();
        Controls.AddRange([_level, _status, _check, close]);
        CancelButton = close;
        _recorder = new MicRecorder(device, silenceThreshold);
        _recorder.Failed += OnFailure;
        _timer.Tick += (_, _) =>
        {
            var level = _recorder.Level;
            _level.Value = Math.Clamp((int)(level * 500), 0, 100);
            _heardAudio |= level > 0 && level >= silenceThreshold;
            var remaining = Math.Max(0, 10 - (int)_clock.Elapsed.TotalSeconds);
            _status.Text = _heardAudio ? $"Audio is reaching speakeasy.  ·  {remaining}s left" : $"Listening for audio…  ·  {remaining}s left";
            if (_clock.Elapsed >= TimeSpan.FromSeconds(10)) FinishCheck();
        };
        Shown += (_, _) => { Theme.Reveal(this, activate: true); StartCheck(); };
        AutoScaleMode = AutoScaleMode.Dpi;
        AutoScaleDimensions = new SizeF(96, 96);
        ResumeLayout(false);
    }

    private void StartCheck()
    {
        _recorder.Cancel();
        _heardAudio = false;
        try
        {
            _recorder.Start();
            _clock.Restart();
            _timer.Start();
            _check.Enabled = false;
            _status.Text = "Listening for audio…";
        }
        catch (Exception exception)
        {
            FinishCheck();
            _status.Text = exception.Message;
        }
    }

    private void FinishCheck()
    {
        _timer.Stop();
        _recorder.Cancel();
        _level.Value = 0;
        _check.Enabled = true;
        _status.Text = _heardAudio ? "Input detected. Your microphone is ready." : "No input detected. Choose another microphone or check\nyour remote audio connection, then try again.";
    }

    private void OnFailure(Exception exception)
    {
        if (_disposed) return;
        FinishCheck();
        _status.Text = exception.Message;
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing && !_disposed)
        {
            _disposed = true;
            _timer.Dispose();
            _recorder.Failed -= OnFailure;
            _recorder.Dispose();
            Icon?.Dispose();
        }
        base.Dispose(disposing);
    }
}
