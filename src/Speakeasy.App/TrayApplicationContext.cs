using System.Diagnostics;
using Microsoft.Win32;
using Speakeasy.App.Platform;
using Speakeasy.App.UI;
using Speakeasy.Core.Configuration;
using Speakeasy.Core.Transcription;

namespace Speakeasy.App;

internal sealed class TrayApplicationContext : ApplicationContext
{
    private readonly ConfigStore _store;
    private readonly string _directory;
    private readonly RecordingPill _pill = new();
    private readonly NotifyIcon _tray;
    private readonly ToolStripMenuItem _enabledItem;
    private readonly ToolStripMenuItem _startupItem;
    private readonly SynchronizationContext _ui;
    private AppSettings _settings;
    private GlobalHotkey? _hotkey;
    private DictationController? _controller;
    private DashboardForm? _dashboard;
    private bool _localReady;
    private bool _disposed;
    private string? _engineStatus;
    private readonly EventWaitHandle _showSignal = new(false, EventResetMode.AutoReset, @"Local\speakeasy.show");
    private readonly RegisteredWaitHandle _showRegistration;
    private readonly EventWaitHandle _quitSignal = new(false, EventResetMode.AutoReset, @"Local\speakeasy.quit");
    private readonly RegisteredWaitHandle _quitRegistration;

    public TrayApplicationContext(ConfigStore store, AppSettings settings, string directory, bool showDashboard)
    {
        _store = store;
        _directory = directory;
        _settings = settings;
        _ui = SynchronizationContext.Current!;
        _showRegistration = ThreadPool.RegisterWaitForSingleObject(_showSignal,
            (_, _) => _ui.Post(_ => { if (!_disposed) ShowDashboard(); }, null), null, Timeout.Infinite, false);
        _quitRegistration = ThreadPool.RegisterWaitForSingleObject(_quitSignal,
            (_, _) => _ui.Post(_ => { if (!_disposed) ExitThread(); }, null), null, Timeout.Infinite, false);
        var menu = new ContextMenuStrip { Font = Theme.Font(10) };
        var title = new ToolStripMenuItem("speakeasy") { Enabled = false };
        _enabledItem = new ToolStripMenuItem("Dictation enabled", null, (_, _) => ToggleEnabled());
        _startupItem = new ToolStripMenuItem("Launch at login", null, (_, _) => SetStartup(!_startupItem!.Checked));
        menu.Items.AddRange([
            title, new ToolStripSeparator(), _enabledItem, _startupItem, new ToolStripSeparator(),
            new ToolStripMenuItem("Open speakeasy", null, (_, _) => ShowDashboard()),
            new ToolStripMenuItem("Preferences", null, (_, _) => ShowPreferences()),
            new ToolStripMenuItem("Edit settings", null, (_, _) => OpenFile(_store.SettingsPath)),
            new ToolStripMenuItem("Edit API keys (.env)", null, (_, _) => OpenFile(_store.EnvPath)),
            new ToolStripMenuItem("Reload settings", null, (_, _) => Reload()),
            new ToolStripSeparator(), new ToolStripMenuItem("Quit", null, (_, _) => ExitThread())
        ]);
        _tray = new NotifyIcon { Icon = Theme.MakeIcon(), Text = "speakeasy", ContextMenuStrip = menu, Visible = true };
        _tray.DoubleClick += (_, _) => ShowDashboard();
        Install(settings);
        SystemEvents.SessionSwitch += OnSessionSwitch;
        SystemEvents.PowerModeChanged += OnPowerModeChanged;
        if (showDashboard) ShowDashboard();
    }

    private void Install(AppSettings settings)
    {
        // Construct first so invalid configuration never leaves a half-installed hotkey.
        settings.Validate();
        var recorder = new MicRecorder(settings.MicrophoneDevice, settings.SilenceThreshold);
        var pipeline = new TranscriptionPipeline(settings, _directory);
        var controller = new DictationController(settings, recorder, pipeline, new ClipboardInserter());
        GlobalHotkey hook;
        try { hook = new GlobalHotkey(settings.Hotkey) { Enabled = false }; }
        catch { controller.Dispose(); throw; }
        _hotkey?.Dispose();
        _controller?.Dispose();
        _settings = settings;
        _controller = controller;
        _hotkey = hook;
        hook.Down += controller.KeyDown;
        hook.Up += controller.KeyUp;
        hook.Escape += controller.Cancel;
        controller.Changed += RefreshStatus;
        controller.Notice += ShowNotice;
        hook.Enabled = settings.Enabled;
        _localReady = File.Exists(Resolve(settings.Transcription.WhisperExecutable)) && File.Exists(Resolve(settings.Transcription.ModelPath));
        _engineStatus = settings.Transcription.Provider == "local" && _localReady ? "Loading local models" : null;
        RefreshStatus();
        if (_settings.Enabled && _localReady) _ = WarmupAsync(pipeline, controller);
    }

    private async Task WarmupAsync(TranscriptionPipeline pipeline, DictationController owner)
    {
        try { await pipeline.WarmupAsync(CancellationToken.None); }
        catch (OperationCanceledException) { }
        catch (Exception exception)
        {
            if (!_disposed && ReferenceEquals(_controller, owner)) ShowNotice(exception.Message);
        }
        finally
        {
            if (!_disposed && ReferenceEquals(_controller, owner))
            {
                _engineStatus = null;
                RefreshStatus();
            }
        }
    }

    private string Resolve(string path) => Path.GetFullPath(path, _directory);

    private void ToggleEnabled()
    {
        RunSafely(() =>
        {
            _settings.Enabled = !_settings.Enabled;
            _store.Save(_settings);
            _controller!.SetEnabled(_settings.Enabled);
            _hotkey!.Enabled = _settings.Enabled;
            RefreshStatus();
        });
    }

    private void Reload() => RunSafely(() =>
    {
        var settings = _store.Load();
        Install(settings);
        ShowNotice("Settings reloaded.");
    });

    private void SetStartup(bool enabled) => RunSafely(() =>
    {
        StartupRegistration.SetEnabled(enabled, _directory);
        RefreshStatus();
    });

    private void RefreshStatus()
    {
        if (_disposed || _controller is null) return;
        _pill.UpdateState(_controller.State, _controller.Status, _controller.Elapsed, _controller.Level);
        _enabledItem.Checked = _settings.Enabled;
        // Registry is read only on refresh outside recording to avoid repeated I/O in animation.
        if (_controller.State == Speakeasy.Core.Interaction.DictationState.Idle)
            _startupItem.Checked = StartupRegistration.IsEnabled();
        var status = _controller.State == Speakeasy.Core.Interaction.DictationState.Idle ? _engineStatus ?? _controller.Status : _controller.Status;
        var tooltip = $"speakeasy · {(_settings.Enabled ? status : "Paused")}";
        _tray.Text = tooltip.Length <= 63 ? tooltip : tooltip[..63];
        if (_dashboard is { IsDisposed: false, Visible: true })
            _dashboard.UpdateSettings(_settings, status, _startupItem.Checked, _localReady);
    }

    private void ShowDashboard()
    {
        if (_dashboard is null || _dashboard.IsDisposed)
        {
            _dashboard = new DashboardForm();
            _dashboard.ToggleRequested += ToggleEnabled;
            _dashboard.SettingsRequested += ShowPreferences;
            _dashboard.EnvRequested += () => OpenFile(_store.EnvPath);
            _dashboard.ReloadRequested += Reload;
            _dashboard.StartupChanged += SetStartup;
        }
        _dashboard.UpdateSettings(_settings, _controller!.Status, StartupRegistration.IsEnabled(), _localReady);
        _dashboard.Show();
        if (_dashboard.WindowState == FormWindowState.Minimized) _dashboard.WindowState = FormWindowState.Normal;
        Theme.Reveal(_dashboard, activate: true);
        _dashboard.Activate();
    }

    private void ShowPreferences()
    {
        _controller?.Cancel();
        if (_hotkey is not null) _hotkey.Enabled = false;
        using var preferences = new PreferencesForm(_settings);
        preferences.AdvancedSettingsRequested += () =>
        {
            preferences.DialogResult = DialogResult.Cancel;
            preferences.Close();
            OpenFile(_store.SettingsPath);
        };
        try
        {
            if (preferences.ShowDialog(_dashboard) == DialogResult.OK && preferences.Result is { } updated)
            {
                Install(updated);
                _store.Save(updated);
            }
        }
        catch (Exception exception) { ShowNotice(exception.Message); }
        finally
        {
            if (_hotkey is not null) _hotkey.Enabled = _settings.Enabled;
            RefreshStatus();
        }
    }

    private void ShowNotice(string message)
    {
        if (_disposed) return;
        _tray.ShowBalloonTip(5000, "speakeasy", message.Length <= 500 ? message : message[..500], ToolTipIcon.Info);
    }

    private void OpenFile(string path) => RunSafely(() =>
    {
        var start = new ProcessStartInfo("notepad.exe") { UseShellExecute = false };
        start.ArgumentList.Add(path);
        Process.Start(start)?.Dispose();
    });

    private void RunSafely(Action action)
    {
        try { action(); }
        catch (Exception exception) { ShowNotice(exception.Message); RefreshStatus(); }
    }

    private void OnSessionSwitch(object sender, SessionSwitchEventArgs args)
    {
        if (args.Reason is SessionSwitchReason.SessionLock or SessionSwitchReason.SessionLogoff
            or SessionSwitchReason.RemoteDisconnect or SessionSwitchReason.ConsoleDisconnect)
            _ui.Post(_ => { if (!_disposed) _controller?.Cancel(); }, null);
        else if (args.Reason is SessionSwitchReason.SessionUnlock or SessionSwitchReason.SessionLogon
                 or SessionSwitchReason.ConsoleConnect or SessionSwitchReason.RemoteConnect)
            _ui.Post(_ => ResetInputAfterResume(), null);
    }

    private void OnPowerModeChanged(object sender, PowerModeChangedEventArgs args)
    {
        if (args.Mode == PowerModes.Suspend) _ui.Post(_ => { if (!_disposed) _controller?.Cancel(); }, null);
        else if (args.Mode == PowerModes.Resume) _ui.Post(_ => ResetInputAfterResume(), null);
    }

    private void ResetInputAfterResume()
    {
        if (_disposed) return;
        _controller?.Cancel();
        _hotkey?.ResetPressedState();
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing && !_disposed)
        {
            _disposed = true;
            _showRegistration.Unregister(null);
            _showSignal.Dispose();
            _quitRegistration.Unregister(null);
            _quitSignal.Dispose();
            SystemEvents.SessionSwitch -= OnSessionSwitch;
            SystemEvents.PowerModeChanged -= OnPowerModeChanged;
            _hotkey?.Dispose();
            _controller?.Dispose();
            _pill.Dispose();
            _dashboard?.Dispose();
            _tray.Visible = false;
            _tray.ContextMenuStrip?.Dispose();
            _tray.Icon?.Dispose();
            _tray.Dispose();
        }
        base.Dispose(disposing);
    }
}
