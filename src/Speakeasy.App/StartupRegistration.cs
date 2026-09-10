using System.Diagnostics;
using System.Reflection;
using Microsoft.Win32;

namespace Speakeasy.App;

internal static class StartupRegistration
{
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string Name = "speakeasy";
    public static bool IsEnabled()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey);
        return key?.GetValue(Name) is string;
    }

    public static void SetEnabled(bool enabled, string configDirectory)
    {
        using var key = Registry.CurrentUser.CreateSubKey(RunKey);
        if (!enabled) { key.DeleteValue(Name, false); return; }
        var executable = Environment.ProcessPath ?? throw new InvalidOperationException("Could not find the app executable.");
        var command = Quote(executable);
        if (Path.GetFileNameWithoutExtension(executable).Equals("dotnet", StringComparison.OrdinalIgnoreCase))
            command += " " + Quote(Assembly.GetExecutingAssembly().Location);
        command += " --config-dir " + Quote(configDirectory);
        key.SetValue(Name, command, RegistryValueKind.String);
    }

    private static string Quote(string value) => "\"" + value.TrimEnd(Path.DirectorySeparatorChar).Replace("\"", "\\\"") + "\"";
}
