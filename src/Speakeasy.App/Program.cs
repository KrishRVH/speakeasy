using Speakeasy.App.UI;
using Speakeasy.App.Diagnostics;
using Speakeasy.App.Platform;
using Speakeasy.Core.Configuration;

namespace Speakeasy.App;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        ApplicationConfiguration.Initialize();
        if (args.Contains("--quit"))
        {
            try { using var quit = EventWaitHandle.OpenExisting(@"Local\speakeasy.quit"); quit.Set(); }
            catch (WaitHandleCannotBeOpenedException) { }
            return 0;
        }
        if (args.Length == 2 && args[0] == "--render-preview")
        {
            PreviewRenderer.Render(Path.GetFullPath(args[1]));
            return 0;
        }
        var directory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "speakeasy");
        var locationFile = Path.Combine(AppContext.BaseDirectory, "config-location.txt");
        if (File.Exists(locationFile)) directory = Path.GetFullPath(File.ReadAllText(locationFile).Trim());
        var showSettings = args.Contains("--settings");
        var configIndex = Array.IndexOf(args, "--config-dir");
        if (configIndex >= 0)
        {
            if (configIndex + 1 >= args.Length)
            {
                MessageBox.Show("--config-dir needs a directory path.", "speakeasy");
                return 1;
            }
            directory = Path.GetFullPath(args[configIndex + 1]);
        }
        var audioIndex = Array.IndexOf(args, "--transcribe-file");
        if (audioIndex >= 0)
        {
            var resultIndex = Array.IndexOf(args, "--result-file");
            if (audioIndex + 1 >= args.Length || resultIndex < 0 || resultIndex + 1 >= args.Length) return 2;
            return TranscriptionProbe.RunAsync(directory, args[audioIndex + 1], args[resultIndex + 1]).GetAwaiter().GetResult();
        }
        using var singleton = new Mutex(true, @"Local\speakeasy.desktop", out var isFirstInstance);
        if (!isFirstInstance)
        {
            try
            {
                using var show = EventWaitHandle.OpenExisting(@"Local\speakeasy.show");
                show.Set();
            }
            catch (WaitHandleCannotBeOpenedException)
            {
                MessageBox.Show("speakeasy is starting. Its icon will appear in your system tray.", "speakeasy");
            }
            return 0;
        }
        try
        {
            var store = new ConfigStore(directory);
            var firstRun = !File.Exists(store.SettingsPath);
            var settings = store.Load();
            _ = HotkeyGesture.Parse(settings.Hotkey);
            SynchronizationContext.SetSynchronizationContext(new WindowsFormsSynchronizationContext());
            using var context = new TrayApplicationContext(store, settings, directory, showSettings || firstRun);
            Application.Run(context);
            return 0;
        }
        catch (Exception exception)
        {
            MessageBox.Show($"speakeasy could not start.\n\n{exception.Message}\n\nSettings: {directory}",
                "speakeasy", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
        finally
        {
            singleton.ReleaseMutex();
        }
    }
}
