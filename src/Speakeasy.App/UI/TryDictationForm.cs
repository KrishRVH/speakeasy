using Speakeasy.Core.Interaction;

namespace Speakeasy.App.UI;

/// <summary>A temporary writing surface using the app's normal dictation and paste path.</summary>
internal sealed class TryDictationForm : Form
{
    private readonly DictationController _controller;
    private readonly TextBox _scratchpad;
    private readonly Label _status;
    private readonly Label _notice;
    private readonly SoftButton _start;
    private readonly SoftButton _finish;
    private readonly SoftButton _clear;
    private DictationState _lastState;
    private bool _disposed;

    public TryDictationForm(DictationController controller, string shortcut)
    {
        _controller = controller;
        _lastState = controller.State;
        SuspendLayout();
        Text = "Try dictation · speakeasy";
        ClientSize = new Size(700, 550);
        BackColor = Theme.Background;
        ForeColor = Theme.Ink;
        Font = Theme.Font(10);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        StartPosition = FormStartPosition.CenterParent;
        Icon = Theme.MakeIcon();
        KeyPreview = true;
        AutoScroll = true;

        Controls.Add(new Label
        {
            Text = "Give your keyboard a break.",
            Location = new Point(24, 20),
            Size = new Size(652, 40),
            Font = Theme.Font(23, FontStyle.Bold)
        });
        Controls.Add(new Label
        {
            Text = $"Click Start dictation, speak, then Finish and insert.\nYou can also hold {shortcut} and release, or double-tap for hands-free.",
            Location = new Point(26, 74),
            Size = new Size(648, 48),
            ForeColor = Theme.Muted
        });
        _status = new Label
        {
            Location = new Point(26, 134),
            Size = new Size(648, 26),
            ForeColor = Theme.Green
        };
        _scratchpad = new TextBox
        {
            Location = new Point(26, 171),
            Size = new Size(648, 198),
            Multiline = true,
            AcceptsReturn = true,
            AcceptsTab = true,
            ScrollBars = ScrollBars.Vertical,
            Font = Theme.Font(12),
            AccessibleName = "Practice dictation text",
            TabIndex = 0,
            PlaceholderText = "Your words will appear here. You can also type or edit them."
        };
        _notice = new Label
        {
            Location = new Point(26, 381),
            Size = new Size(648, 43),
            ForeColor = Theme.Muted,
            UseMnemonic = false,
            AutoEllipsis = true
        };
        _start = new SoftButton
        {
            Text = "Start dictation",
            Location = new Point(26, 438),
            Size = new Size(156, 44),
            Primary = true,
            TabIndex = 1
        };
        _finish = new SoftButton
        {
            Text = "Finish and insert",
            Location = new Point(194, 438),
            Size = new Size(174, 44),
            Primary = true,
            TabIndex = 2
        };
        _clear = new SoftButton
        {
            Text = "Clear text",
            Location = new Point(380, 438),
            Size = new Size(142, 44),
            TabIndex = 3
        };
        var close = new SoftButton
        {
            Text = "Close",
            Location = new Point(534, 438),
            Size = new Size(140, 44),
            TabIndex = 4
        };
        _start.Click += (_, _) => ToggleRecording();
        _finish.Click += (_, _) => ToggleRecording();
        _clear.Click += (_, _) => { _scratchpad.Clear(); _scratchpad.Focus(); };
        close.Click += (_, _) => Close();
        Controls.AddRange([_status, _scratchpad, _notice, _start, _finish, _clear, close]);
        Controls.Add(new Label
        {
            Text = "This practice text is not saved. Your normal provider and clipboard settings apply.",
            Location = new Point(26, 502),
            Size = new Size(648, 30),
            ForeColor = Theme.Muted,
            Font = Theme.Font(9)
        });
        _controller.Changed += RefreshStatus;
        Shown += (_, _) => { Theme.Reveal(this, activate: true); _scratchpad.Focus(); };
        FormClosing += (_, _) => _controller.Cancel();
        AutoScaleMode = AutoScaleMode.Dpi;
        AutoScaleDimensions = new SizeF(96, 96);
        ResumeLayout(false);
        RefreshStatus();
    }

    private void ToggleRecording()
    {
        // The real inserter targets the focused control; keep the selected text/caret.
        _scratchpad.Focus();
        _controller.ToggleHandsFree();
    }

    private void RefreshStatus()
    {
        if (_disposed) return;
        var state = _controller.State;
        var recording = state is DictationState.Held or DictationState.PendingTap or DictationState.HandsFree;
        _notice.Text = _controller.LastNotice ?? "";
        if (state == DictationState.Processing && _lastState != state && ContainsFocus) _scratchpad.Focus();
        _lastState = state;
        _status.Text = !_controller.IsEnabled ? "Dictation is paused. Enable it from the tray to begin."
            : recording ? $"{_controller.Status}  ·  {_controller.Elapsed:mm\\:ss}  ·  Esc cancels"
            : _controller.Status;
        _start.Enabled = _controller.IsEnabled && state == DictationState.Idle;
        _finish.Enabled = recording;
        _clear.Enabled = state == DictationState.Idle;
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (e.KeyCode == Keys.Escape)
        {
            _controller.Cancel();
            _scratchpad.Focus();
        }
        // Escape continues through normal WinForms/textbox handling.
        base.OnKeyDown(e);
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing && !_disposed)
        {
            _disposed = true;
            _controller.Changed -= RefreshStatus;
            _controller.Cancel();
            Icon?.Dispose();
        }
        base.Dispose(disposing);
    }
}
