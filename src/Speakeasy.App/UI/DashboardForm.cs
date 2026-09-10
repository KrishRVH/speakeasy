using Speakeasy.Core.Configuration;

namespace Speakeasy.App.UI;

internal sealed class DashboardForm : Form
{
    private readonly Label _status;
    private readonly Label _provider;
    private readonly Label _hotkey;
    private readonly Label _note;
    private readonly SoftButton _toggle;
    private readonly CheckBox _startup;
    private bool _updating;
    public event Action? ToggleRequested;
    public event Action? SettingsRequested;
    public event Action? EnvRequested;
    public event Action? ReloadRequested;
    public event Action? TryRequested;
    public event Action<bool>? StartupChanged;

    public DashboardForm()
    {
        SuspendLayout();
        Text = "speakeasy";
        Icon = Theme.MakeIcon();
        BackColor = Theme.Background;
        ForeColor = Theme.Ink;
        Font = Theme.Font(10);
        FormBorderStyle = FormBorderStyle.FixedSingle;
        MaximizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        ClientSize = new Size(700, 640);
        DoubleBuffered = true;

        AddLabel("speakeasy", 79, 29, 230, 38, 21, FontStyle.Bold);
        AddLabel("A little less typing. A little more you.", 35, 103, 630, 39, 22, FontStyle.Bold);
        AddLabel("Your voice, wherever your cursor is.", 37, 152, 590, 27, 11, color: Theme.Muted);

        var statusCard = new Panel { Location = new Point(36, 207), Size = new Size(628, 84), BackColor = Color.FromArgb(234, 240, 228) };
        _status = new Label { Location = new Point(19, 15), Size = new Size(420, 26), Font = Theme.Font(12, FontStyle.Bold), Text = "Ready when you are" };
        _provider = new Label { Location = new Point(19, 45), Size = new Size(580, 24), ForeColor = Theme.Green };
        statusCard.Controls.AddRange([_status, _provider]);
        Controls.Add(statusCard);

        AddLabel("01", 37, 321, 32, 26, 10, FontStyle.Bold, Theme.Green);
        AddLabel("Hold to talk", 82, 314, 228, 30, 13, FontStyle.Bold);
        _hotkey = AddLabel("Ctrl + Alt + Space", 82, 350, 236, 29, 11, color: Theme.Green);
        AddLabel("Release to transcribe and paste.", 82, 386, 240, 28, 9, color: Theme.Muted);

        AddLabel("02", 364, 321, 32, 26, 10, FontStyle.Bold, Theme.Green);
        AddLabel("Or go hands-free", 408, 314, 238, 30, 13, FontStyle.Bold);
        AddLabel("Double-tap your shortcut.", 408, 350, 240, 29, 11);
        AddLabel("Tap again to finish. Esc to cancel.", 408, 386, 245, 28, 9, color: Theme.Muted);

        _note = AddLabel("Local by default. No accounts. No telemetry.", 37, 441, 628, 43, 9, color: Theme.Muted);
        _note.AutoEllipsis = true;
        _note.UseMnemonic = false;
        _startup = new CheckBox { Text = "Launch at login", Location = new Point(39, 499), Size = new Size(194, 30), Cursor = Cursors.Hand };
        _startup.CheckedChanged += (_, _) => { if (!_updating) StartupChanged?.Invoke(_startup.Checked); };
        Controls.Add(_startup);

        _toggle = AddButton("Pause dictation", 36, 550, 132, true, () => ToggleRequested?.Invoke());
        AddButton("Preferences", 180, 550, 132, false, () => SettingsRequested?.Invoke());
        AddButton("Try dictation", 324, 550, 132, false, () => TryRequested?.Invoke());
        AddButton("API keys", 468, 550, 100, false, () => EnvRequested?.Invoke());
        AddButton("Reload", 580, 550, 84, false, () => ReloadRequested?.Invoke());
        AddLabel("Lives in your system tray. Close this window and keep speaking.", 37, 608, 628, 22, 8, color: Theme.Muted);
        AutoScaleMode = AutoScaleMode.Dpi;
        AutoScaleDimensions = new SizeF(96, 96);
        ResumeLayout(false);
        PerformLayout();
    }

    public void UpdateSettings(AppSettings settings, string status, bool startupEnabled, bool localReady, string? notice = null)
    {
        _updating = true;
        _status.Text = settings.Enabled ? status : "Dictation is paused";
        _provider.Text = settings.Transcription.Provider.ToLowerInvariant() switch
        {
            "local" => localReady ? "●  Local Whisper  ·  On this device" : "○  Local Whisper  ·  Model setup needed",
            "groq" => "●  Groq Whisper  ·  Cloud transcription",
            _ => "●  OpenAI Whisper  ·  Cloud transcription"
        };
        _hotkey.Text = settings.Hotkey.Replace("+", " + ");
        _toggle.Text = settings.Enabled ? "Pause dictation" : "Enable dictation";
        _startup.Checked = startupEnabled;
        var limit = settings.MaxRecordingSeconds < 60
            ? $"{settings.MaxRecordingSeconds} seconds" : $"{settings.MaxRecordingSeconds / 60.0:0.#} minutes";
        _note.Text = !string.IsNullOrWhiteSpace(notice) ? notice
            : settings.Transcription.Provider.Equals("local", StringComparison.OrdinalIgnoreCase) && !localReady
            ? "Run scripts/setup-local.ps1 to install whisper.cpp and a model.\nYour shortcut and API preferences live in the settings file."
            : $"Recording limit: {limit}. Esc always passes through to your app.\nNo account or telemetry. Cloud providers receive audio/text only when configured.";
        _updating = false;
        Invalidate();
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        base.OnPaint(e);
        e.Graphics.ScaleTransform(DeviceDpi / 96f, DeviceDpi / 96f);
        Theme.Logo(e.Graphics, new Rectangle(37, 38, 29, 26), Theme.Green);
        using var pen = new Pen(Theme.Line);
        e.Graphics.DrawLine(pen, 348, 323, 348, 405);
        e.Graphics.DrawLine(pen, 36, 427, 664, 427);
    }

    private Label AddLabel(string text, int x, int y, int width, int height, float size,
        FontStyle style = FontStyle.Regular, Color? color = null)
    {
        var label = new Label
        {
            Text = text,
            Location = new Point(x, y),
            Size = new Size(width, height),
            Font = Theme.Font(size, style),
            ForeColor = color ?? Theme.Ink
        };
        Controls.Add(label);
        return label;
    }

    private SoftButton AddButton(string text, int x, int y, int width, bool primary, Action action)
    {
        var button = new SoftButton { Text = text, Location = new Point(x, y), Width = width, Primary = primary };
        button.Click += (_, _) => action();
        Controls.Add(button);
        return button;
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing) Icon?.Dispose();
        base.Dispose(disposing);
    }
}
