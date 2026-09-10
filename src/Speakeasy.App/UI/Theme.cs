using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;

namespace Speakeasy.App.UI;

internal static class Theme
{
    internal static readonly Color Background = Color.FromArgb(248, 247, 243);
    internal static readonly Color Ink = Color.FromArgb(30, 42, 38);
    internal static readonly Color Muted = Color.FromArgb(111, 119, 114);
    internal static readonly Color Green = Color.FromArgb(45, 101, 77);
    internal static readonly Color Mint = Color.FromArgb(192, 233, 199);
    internal static readonly Color Line = Color.FromArgb(225, 228, 220);
    internal static Font Font(float size, FontStyle style = FontStyle.Regular) => new("Segoe UI", size, style);

    internal static GraphicsPath Rounded(RectangleF rectangle, float radius)
    {
        var path = new GraphicsPath();
        var diameter = Math.Min(radius * 2, Math.Min(rectangle.Width, rectangle.Height));
        path.AddArc(rectangle.X, rectangle.Y, diameter, diameter, 180, 90);
        path.AddArc(rectangle.Right - diameter, rectangle.Y, diameter, diameter, 270, 90);
        path.AddArc(rectangle.Right - diameter, rectangle.Bottom - diameter, diameter, diameter, 0, 90);
        path.AddArc(rectangle.X, rectangle.Bottom - diameter, diameter, diameter, 90, 90);
        path.CloseFigure();
        return path;
    }

    internal static void Logo(Graphics graphics, Rectangle bounds, Color color)
    {
        graphics.SmoothingMode = SmoothingMode.AntiAlias;
        using var pen = new Pen(color, bounds.Width / 8f) { StartCap = LineCap.Round, EndCap = LineCap.Round };
        var pattern = new[] { .30f, .65f, 1f, .65f, .30f };
        for (var i = 0; i < pattern.Length; i++)
        {
            var x = bounds.Left + bounds.Width * (i + .5f) / pattern.Length;
            var height = bounds.Height * pattern[i];
            graphics.DrawLine(pen, x, bounds.Top + (bounds.Height - height) / 2, x, bounds.Top + (bounds.Height + height) / 2);
        }
    }

    internal static Icon MakeIcon()
    {
        using var bitmap = new Bitmap(64, 64);
        using (var graphics = Graphics.FromImage(bitmap))
        {
            graphics.SmoothingMode = SmoothingMode.AntiAlias;
            using var brush = new SolidBrush(Ink);
            graphics.FillEllipse(brush, 1, 1, 62, 62);
            Logo(graphics, new Rectangle(15, 17, 34, 30), Mint);
        }
        var handle = bitmap.GetHicon();
        try { using var icon = Icon.FromHandle(handle); return (Icon)icon.Clone(); }
        finally { DestroyIcon(handle); }
    }

    // A hidden launcher can suppress the first native ShowWindow call even though
    // WinForms considers the form visible. Explicitly reveal our UI after Show().
    internal static void Reveal(Form form, bool activate) => ShowWindow(form.Handle, activate ? 5 : 4);

    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr window, int command);

    [DllImport("user32.dll")] private static extern bool DestroyIcon(IntPtr handle);
}

internal sealed class SoftButton : Button
{
    [System.ComponentModel.DefaultValue(false)]
    public bool Primary { get; set; }
    public SoftButton()
    {
        FlatStyle = FlatStyle.Flat;
        FlatAppearance.BorderSize = 0;
        Font = Theme.Font(10, FontStyle.Bold);
        Cursor = Cursors.Hand;
        Height = 42;
        SetStyle(ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer, true);
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        e.Graphics.SmoothingMode = SmoothingMode.AntiAlias;
        using var shape = Theme.Rounded(new RectangleF(0, 0, Width - 1, Height - 1), 9);
        using var brush = new SolidBrush(Primary ? Theme.Green : Color.FromArgb(235, 238, 229));
        e.Graphics.FillPath(brush, shape);
        TextRenderer.DrawText(e.Graphics, Text, Font, ClientRectangle, Primary ? Color.White : Theme.Ink,
            TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter);
        if (Focused) ControlPaint.DrawFocusRectangle(e.Graphics, Rectangle.Inflate(ClientRectangle, -5, -5));
    }
}
