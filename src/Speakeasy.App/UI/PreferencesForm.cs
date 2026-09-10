using System.ComponentModel;
using System.Text.Json;
using NAudio.Wave;
using Speakeasy.App.Platform;
using Speakeasy.Core.Configuration;

namespace Speakeasy.App.UI;

internal sealed class PreferencesForm : Form
{
    private readonly AppSettings _edited;
    private readonly TextBox _shortcut;
    private readonly ComboBox _microphone;
    private readonly ComboBox _transcription;
    private readonly ComboBox _cleanup;
    private readonly ComboBox _casing;
    private readonly NumericUpDown _recordingLimit;
    private readonly CheckBox _restoreClipboard;

    [Browsable(false)]
    [DesignerSerializationVisibility(DesignerSerializationVisibility.Hidden)]
    public AppSettings? Result { get; private set; }

    public event Action? AdvancedSettingsRequested;

    public PreferencesForm(AppSettings settings, bool discoverMicrophones = true)
    {
        SuspendLayout();
        _edited = JsonSerializer.Deserialize<AppSettings>(JsonSerializer.Serialize(settings))
            ?? throw new InvalidOperationException("Settings could not be copied.");
        Text = "Preferences · speakeasy";
        ClientSize = new Size(700, 640);
        BackColor = Theme.Background;
        ForeColor = Theme.Ink;
        Font = Theme.Font(10);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        ShowInTaskbar = false;
        StartPosition = FormStartPosition.CenterParent;
        Icon = Theme.MakeIcon();
        AutoScroll = true;

        AddLabel(this, "Make it yours.", 24, 20, 652, 38, 24, FontStyle.Bold);
        AddLabel(this, "A few preferences for a little less typing.", 26, 65, 648, 22, 10, color: Theme.Muted);

        var capture = Card("RECORDING", 94, 186);
        AddLabel(capture, "&Shortcut", 18, 43, 287, 22);
        _shortcut = new TextBox
        {
            Text = settings.Hotkey,
            Location = new Point(18, 68),
            Width = 290,
            Font = new Font("Consolas", 11),
            AccessibleName = "Dictation shortcut",
            TabIndex = 0
        };
        capture.Controls.Add(_shortcut);
        AddLabel(capture, "Hold to talk. Double-tap for hands-free.\nExamples: Ctrl+Alt+Space, F13, RightAlt.", 18, 101, 305, 37, 9, color: Theme.Muted);

        AddLabel(capture, "&Microphone", 336, 43, 290, 22);
        _microphone = Dropdown(capture, "Microphone", 336, 68, 296, 1);
        _microphone.Items.Add(new DeviceChoice(-1, "Windows default microphone"));
        var microphoneHelp = "Fn usually needs a keyboard-level remap\nto F13. Left/Right modifiers are supported.";
        if (discoverMicrophones)
        {
            try
            {
                for (var index = 0; index < WaveInEvent.DeviceCount; index++)
                    _microphone.Items.Add(new DeviceChoice(index, WaveInEvent.GetCapabilities(index).ProductName));
            }
            catch (Exception ex) when (ex is NAudio.MmException or InvalidOperationException)
            {
                microphoneHelp = "Microphone list unavailable.\nCheck Windows sound settings.";
            }
        }
        var selectedDevice = _microphone.Items.Cast<DeviceChoice>().FirstOrDefault(item => item.Number == settings.MicrophoneDevice);
        if (selectedDevice == null)
        {
            selectedDevice = new DeviceChoice(settings.MicrophoneDevice, $"Device {settings.MicrophoneDevice} (currently unavailable)");
            _microphone.Items.Add(selectedDevice);
        }
        _microphone.SelectedItem = selectedDevice;
        AddLabel(capture, microphoneHelp, 336, 101, 296, 37, 9, color: Theme.Muted);

        AddLabel(capture, "Stop forgotten recordings after", 18, 149, 259, 24, 9);
        _recordingLimit = new NumericUpDown
        {
            Location = new Point(281, 146),
            Width = 73,
            Minimum = 1,
            Maximum = 300,
            Value = Math.Clamp(settings.MaxRecordingSeconds, 1, 300),
            AccessibleName = "Maximum recording length in seconds",
            TabIndex = 2
        };
        capture.Controls.Add(_recordingLimit);
        AddLabel(capture, "seconds  ·  up to 5 minutes", 365, 149, 267, 24, 9, color: Theme.Muted);

        var intelligence = Card("TRANSCRIPTION && CLEANUP", 292, 132);
        AddLabel(intelligence, "&Transcribe with", 18, 42, 290, 23);
        _transcription = Dropdown(intelligence, "Transcription provider", 18, 68, 290, 3);
        SetChoices(_transcription, settings.Transcription.Provider,
            new("local", "On-device · whisper.cpp"), new("groq", "Groq · Whisper"), new("openai", "OpenAI · Whisper"));
        AddLabel(intelligence, "&Clean up with", 336, 42, 296, 23);
        _cleanup = Dropdown(intelligence, "Text cleanup provider", 336, 68, 296, 4);
        SetChoices(_cleanup, settings.Cleanup.Provider,
            new("auto", "Automatic"), new("none", "Off · use raw transcript"),
            new("local", "Local LLM"), new("groq", "Groq"), new("openai", "OpenAI"));
        AddLabel(intelligence, "Cloud providers use your API keys in .env.", 18, 103, 306, 21, 9, color: Theme.Muted);
        AddLabel(intelligence, "Models and local setup live in advanced settings.", 336, 103, 296, 21, 8.5f, color: Theme.Muted);

        var output = Card("YOUR WORDS", 436, 110);
        AddLabel(output, "&Casing", 18, 40, 195, 23);
        _casing = Dropdown(output, "Dictation casing", 18, 65, 221, 5);
        SetChoices(_casing, settings.Cleanup.Style,
            new("natural", "Natural"), new("sentence", "Sentence case"), new("lowercase", "lowercase"));
        _restoreClipboard = new CheckBox
        {
            Text = "&Restore my previous clipboard",
            Checked = settings.RestoreClipboard,
            Location = new Point(280, 39),
            Size = new Size(352, 26),
            AccessibleName = "Restore previous clipboard after pasting",
            TabIndex = 6
        };
        output.Controls.Add(_restoreClipboard);
        AddLabel(output, "Otherwise, your dictated text stays copied\nso you can paste it again.", 301, 68, 331, 34, 9, color: Theme.Muted);

        var advanced = new LinkLabel
        {
            Text = "Advanced settings file ↗",
            Location = new Point(26, 581),
            AutoSize = true,
            Font = Theme.Font(10),
            LinkColor = Theme.Green,
            ActiveLinkColor = Theme.Ink,
            VisitedLinkColor = Theme.Green,
            LinkBehavior = LinkBehavior.HoverUnderline,
            TabIndex = 7,
            AccessibleName = "Open advanced settings file"
        };
        advanced.LinkClicked += (_, _) => AdvancedSettingsRequested?.Invoke();
        Controls.Add(advanced);
        var cancel = new SoftButton
        {
            Text = "Cancel",
            Location = new Point(439, 568),
            Size = new Size(104, 44),
            DialogResult = DialogResult.Cancel,
            TabIndex = 8
        };
        var save = new SoftButton
        {
            Text = "Save changes",
            Primary = true,
            Location = new Point(555, 568),
            Size = new Size(121, 44),
            TabIndex = 9
        };
        cancel.Click += (_, _) => Close();
        save.Click += (_, _) => SaveChanges();
        Shown += (_, _) => Theme.Reveal(this, activate: true);
        Controls.Add(cancel);
        Controls.Add(save);
        AcceptButton = save;
        CancelButton = cancel;
        AutoScaleMode = AutoScaleMode.Dpi;
        AutoScaleDimensions = new SizeF(96, 96);
        ResumeLayout(false);
        PerformLayout();
    }

    private void SaveChanges()
    {
        try
        {
            _edited.Hotkey = _shortcut.Text.Trim();
            HotkeyGesture.Parse(_edited.Hotkey);
            _edited.MicrophoneDevice = ((DeviceChoice)_microphone.SelectedItem!).Number;
            _edited.MaxRecordingSeconds = (int)_recordingLimit.Value;
            var transcriptionProvider = ((Choice)_transcription.SelectedItem!).Value;
            var cleanupProvider = ((Choice)_cleanup.SelectedItem!).Value;
            if (_edited.Transcription.Provider != transcriptionProvider) _edited.Transcription.Model = "";
            if (_edited.Cleanup.Provider != cleanupProvider) _edited.Cleanup.Model = "";
            _edited.Transcription.Provider = transcriptionProvider;
            _edited.Cleanup.Provider = cleanupProvider;
            _edited.Cleanup.Style = ((Choice)_casing.SelectedItem!).Value;
            _edited.RestoreClipboard = _restoreClipboard.Checked;
            _edited.Validate();
            Result = _edited;
            DialogResult = DialogResult.OK;
            Close();
        }
        catch (Exception ex) when (ex is ConfigurationException or ArgumentException)
        {
            MessageBox.Show(this, ex.Message, "Check your preferences", MessageBoxButtons.OK, MessageBoxIcon.Information);
        }
    }

    private Panel Card(string title, int top, int height)
    {
        var panel = new Panel { Location = new Point(24, top), Size = new Size(652, height), BackColor = Color.White };
        panel.Paint += (_, e) =>
        {
            using var pen = new Pen(Theme.Line);
            e.Graphics.DrawRectangle(pen, 0, 0, panel.Width - 1, panel.Height - 1);
        };
        AddLabel(panel, title, 18, 13, 614, 23, 9, FontStyle.Bold, Theme.Green);
        Controls.Add(panel);
        return panel;
    }

    private static ComboBox Dropdown(Control parent, string name, int left, int top, int width, int tabIndex)
    {
        var dropdown = new ComboBox
        {
            Location = new Point(left, top),
            Width = width,
            DropDownStyle = ComboBoxStyle.DropDownList,
            FlatStyle = FlatStyle.Standard,
            AccessibleName = name,
            TabIndex = tabIndex
        };
        parent.Controls.Add(dropdown);
        return dropdown;
    }

    private static void SetChoices(ComboBox dropdown, string selected, params Choice[] choices)
    {
        dropdown.Items.AddRange(choices);
        dropdown.SelectedItem = choices.FirstOrDefault(choice => choice.Value.Equals(selected, StringComparison.OrdinalIgnoreCase)) ?? choices[0];
    }

    private static void AddLabel(Control parent, string text, int x, int y, int width, int height,
        float size = 10, FontStyle style = FontStyle.Regular, Color? color = null)
    {
        parent.Controls.Add(new Label
        {
            Text = text,
            Location = new Point(x, y),
            Size = new Size(width, height),
            Font = Theme.Font(size, style),
            ForeColor = color ?? Theme.Ink,
            BackColor = Color.Transparent,
            UseMnemonic = true,
            TabStop = false
        });
    }

    private sealed record DeviceChoice(int Number, string Label)
    {
        public override string ToString() => Label;
    }

    private sealed record Choice(string Value, string Label)
    {
        public override string ToString() => Label;
    }
}
