using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;
using Speakeasy.Core.Interaction;

namespace Speakeasy.App.UI;

internal sealed class RecordingPill : Form
{
    private DictationState _state;
    private string _status = "Listening";
    private TimeSpan _elapsed;
    private float _level;
    private int _frame;
    public RecordingPill()
    {
        SuspendLayout();
        FormBorderStyle = FormBorderStyle.None;
        ShowInTaskbar = false;
        TopMost = true;
        BackColor = Theme.Ink;
        ClientSize = new Size(332, 66);
        StartPosition = FormStartPosition.Manual;
        DoubleBuffered = true;
        AccessibleName = "speakeasy recording status";
        AutoScaleMode = AutoScaleMode.Dpi;
        AutoScaleDimensions = new SizeF(96, 96);
        ResumeLayout(false);
    }

    protected override bool ShowWithoutActivation => true;
    protected override CreateParams CreateParams
    {
        get
        {
            var parameters = base.CreateParams;
            parameters.ExStyle |= 0x08000000 | 0x00000080 | 0x00000020; // NOACTIVATE | TOOLWINDOW | TRANSPARENT
            return parameters;
        }
    }

    public void UpdateState(DictationState state, string status, TimeSpan elapsed, float level)
    {
        _state = state;
        _status = status;
        _elapsed = elapsed;
        _level = level;
        _frame++;
        AccessibleDescription = $"{status}. {elapsed:mm\\:ss}. Escape cancels.";
        if (state == DictationState.Idle) { Hide(); return; }
        if (!Visible)
        {
            var area = Screen.FromHandle(GetForegroundWindow()).WorkingArea;
            Location = new Point(area.Left + (area.Width - Width) / 2, area.Bottom - Height - 26);
            Show();
            Theme.Reveal(this, activate: false);
            // Showing on another display can change the DPI and window dimensions.
            Location = new Point(area.Left + (area.Width - Width) / 2, area.Bottom - Height - 26 * DeviceDpi / 96);
        }
        Invalidate();
    }

    internal void SetPreview(DictationState state, string status, TimeSpan elapsed, float level)
    {
        _state = state; _status = status; _elapsed = elapsed; _level = level;
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        var graphics = e.Graphics;
        graphics.SmoothingMode = SmoothingMode.AntiAlias;
        graphics.Clear(Theme.Ink);
        var processing = _state == DictationState.Processing;
        var scale = DeviceDpi / 96f;
        Rectangle Box(int x, int y, int width, int height) => new((int)(x * scale), (int)(y * scale), (int)(width * scale), (int)(height * scale));
        using var waveform = new Pen(processing ? Color.FromArgb(211, 197, 237) : Theme.Mint, 3 * scale)
        { StartCap = LineCap.Round, EndCap = LineCap.Round };
        for (var i = 0; i < 7; i++)
        {
            var variation = .35 + .65 * Math.Abs(Math.Sin(_frame * .17 + i * 1.3));
            var height = processing ? 5 + 14 * variation : 4 + 27 * Math.Min(1, _level * 5) * variation;
            graphics.DrawLine(waveform, (23 + i * 5) * scale, (33 - (float)height / 2) * scale,
                (23 + i * 5) * scale, (33 + (float)height / 2) * scale);
        }
        using var title = Theme.Font(10, FontStyle.Bold);
        using var detail = Theme.Font(8);
        var text = processing ? _status : _state == DictationState.HandsFree ? "Hands-free" : "Listening";
        TextRenderer.DrawText(graphics, text, title, Box(71, 12, 183, 23), Color.White,
            TextFormatFlags.EndEllipsis | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding);
        var hint = processing ? "Turning speech into words" : _state == DictationState.HandsFree ? "Tap shortcut to finish" : "Release shortcut to finish";
        TextRenderer.DrawText(graphics, hint, detail, Box(71, 35, 189, 18), Color.FromArgb(179, 195, 184), TextFormatFlags.NoPadding);
        TextRenderer.DrawText(graphics, processing ? "•••" : _elapsed.ToString(@"mm\:ss"), title,
            Box(261, 13, 57, 22), Theme.Mint, TextFormatFlags.HorizontalCenter);
        TextRenderer.DrawText(graphics, "esc cancel", detail, Box(257, 36, 65, 19), Color.FromArgb(179, 195, 184),
            TextFormatFlags.HorizontalCenter | TextFormatFlags.NoPadding);
    }

    protected override void OnSizeChanged(EventArgs e)
    {
        base.OnSizeChanged(e);
        if (ClientSize.Width == 0 || ClientSize.Height == 0) return;
        using var shape = Theme.Rounded(ClientRectangle, 22 * DeviceDpi / 96f);
        var oldRegion = Region;
        Region = new Region(shape);
        oldRegion?.Dispose();
    }

    protected override void WndProc(ref Message message)
    {
        if (message.Msg == 0x0084) { message.Result = new IntPtr(-1); return; } // HTTRANSPARENT
        if (message.Msg == 0x0021) { message.Result = new IntPtr(3); return; } // MA_NOACTIVATE
        base.WndProc(ref message);
    }

    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
}
