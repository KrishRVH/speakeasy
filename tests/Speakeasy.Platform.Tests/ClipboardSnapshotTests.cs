using System.Collections.Specialized;
using System.Drawing;
using Speakeasy.App.Platform;

namespace Speakeasy.Platform.Tests;

public sealed class ClipboardSnapshotTests
{
    [Fact]
    public Task MaterializesTextHtmlRtfImageAndFilesWithoutReadingOrWritingSystemClipboard() => RunStaAsync(() =>
    {
        using var image = new Bitmap(2, 2);
        var data = new DataObject();
        data.SetData(DataFormats.UnicodeText, false, "hello");
        data.SetData(DataFormats.Html, false, "<b>hello</b>");
        data.SetData(DataFormats.Rtf, false, "{\\rtf1 hello}");
        data.SetImage(image);
        data.SetFileDropList(new StringCollection { @"C:\test\example.txt" });
        using var snapshot = ClipboardSnapshot.CaptureFrom(data);
        Assert.Equal(data.GetFormats(false).Length, snapshot.FormatCount);
    });

    [Fact]
    public Task PreservesCustomRawFormatsAndOriginalStreamPosition() => RunStaAsync(() =>
    {
        using var stream = new MemoryStream([1, 2, 3, 4]);
        stream.Position = 2;
        var data = new DataObject();
        data.SetData("Speakeasy.UnitTest.Raw", false, stream);
        using var snapshot = ClipboardSnapshot.CaptureFrom(data);
        Assert.Equal(1, snapshot.FormatCount);
        Assert.Equal(2, stream.Position);
        Assert.True(stream.CanRead);
    });

    [Fact]
    public Task RejectsUnsupportedObjectBeforeAnyClipboardReplacement() => RunStaAsync(() =>
    {
        var data = new DataObject();
        data.SetData("Speakeasy.UnitTest.Unsupported", false, new object());
        var error = Assert.Throws<NotSupportedException>(() => ClipboardSnapshot.CaptureFrom(data));
        Assert.Contains("original clipboard was preserved", error.Message);
    });

    private static Task RunStaAsync(Action action)
    {
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() =>
        {
            try { action(); completion.TrySetResult(); }
            catch (Exception ex) { completion.TrySetException(ex); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        return completion.Task;
    }
}
