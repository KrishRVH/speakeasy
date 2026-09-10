using System.Drawing.Imaging;
using Speakeasy.Core.Configuration;
using Speakeasy.Core.Interaction;

namespace Speakeasy.App.UI;

internal static class PreviewRenderer
{
    public static void Render(string directory)
    {
        Directory.CreateDirectory(directory);
        using var dashboard = new DashboardForm();
        dashboard.UpdateSettings(new AppSettings(), "Ready when you are", false, true);
        dashboard.FormBorderStyle = FormBorderStyle.None;
        dashboard.StartPosition = FormStartPosition.Manual;
        dashboard.Location = new Point(-20000, -20000);
        dashboard.Show();
        Application.DoEvents();
        Save(dashboard, Path.Combine(directory, "speakeasy-dashboard.png"));
        using var pill = new RecordingPill();
        pill.SetPreview(DictationState.HandsFree, "Listening", TimeSpan.FromSeconds(42), .14f);
        pill.Location = new Point(-20000, -20000);
        pill.Show();
        Application.DoEvents();
        Save(pill, Path.Combine(directory, "speakeasy-pill.png"));
        using var preferences = new PreferencesForm(new AppSettings(), discoverMicrophones: false);
        preferences.FormBorderStyle = FormBorderStyle.None;
        preferences.StartPosition = FormStartPosition.Manual;
        preferences.Location = new Point(-20000, -20000);
        preferences.Show();
        Application.DoEvents();
        Save(preferences, Path.Combine(directory, "speakeasy-preferences.png"));
        using var icon = Theme.MakeIcon();
        using var iconFile = File.Create(Path.Combine(directory, "speakeasy.ico"));
        icon.Save(iconFile);
    }

    private static void Save(Control control, string path)
    {
        using var bitmap = new Bitmap(control.Width, control.Height);
        control.DrawToBitmap(bitmap, new Rectangle(Point.Empty, control.Size));
        bitmap.Save(path, ImageFormat.Png);
    }
}
