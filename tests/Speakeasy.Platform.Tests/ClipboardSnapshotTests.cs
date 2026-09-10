using System.Collections.Specialized;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using Speakeasy.App.Platform;
using ComDataObject = System.Runtime.InteropServices.ComTypes.IDataObject;

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

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public Task PreservesOpaqueNativeFormatsOrRejectsAnUnavailableNativePayload(bool nativeAvailable) => RunStaAsync(() =>
    {
        var data = new OpaqueNativeDataObject(nativeAvailable);
        if (nativeAvailable)
        {
            using var snapshot = ClipboardSnapshot.CaptureFrom(data);
            Assert.Equal(2, snapshot.FormatCount);
        }
        else
        {
            var error = Assert.Throws<COMException>(() => ClipboardSnapshot.CaptureFrom(data));
            Assert.Equal(unchecked((int)0x80040064), error.HResult);
        }
        Assert.Equal(1, data.NativeReads);
    });

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public Task NullOleMemoryHandleUsesNativeFallbackWithoutDroppingTheFormat(bool nativeAvailable) => RunStaAsync(() =>
    {
        var data = new OpaqueNativeDataObject(true, emptyNativeHandle: true);
        var borrowedHandle = ClipboardNative.GlobalAlloc(0x42, 5);
        Assert.NotEqual(0, borrowedHandle);
        var nativeCopies = 0;
        ClipboardFormatHandle CopyNative(ushort format)
        {
            nativeCopies++;
            Assert.Equal("Speakeasy.UnitTest.OpaqueNative", DataFormats.GetFormat(format).Name);
            if (!nativeAvailable) throw new ExternalException("Native clipboard payload unavailable.");
            return ClipboardFormatHandle.Duplicate(format, borrowedHandle, TYMED.TYMED_HGLOBAL);
        }
        try
        {
            if (nativeAvailable)
            {
                using var snapshot = ClipboardSnapshot.CaptureFrom(data, CopyNative);
                Assert.Equal(2, snapshot.FormatCount);
            }
            else
            {
                var error = Assert.Throws<ExternalException>(() => ClipboardSnapshot.CaptureFrom(data, CopyNative));
                Assert.Equal("Native clipboard payload unavailable.", error.Message);
            }
            Assert.Equal(1, nativeCopies);
            Assert.Equal(1, data.NativeReads);
            var pointer = ClipboardNative.GlobalLock(borrowedHandle);
            Assert.NotEqual(0, pointer);
            ClipboardNative.GlobalUnlock(borrowedHandle);
        }
        finally { ClipboardNative.GlobalFree(borrowedHandle); }
    });

    [Theory]
    [InlineData(2)]
    [InlineData(0x82)]
    public void BitmapCopyCanBeDisposedWithoutDestroyingItsSource(int format)
    {
        using var image = new Bitmap(3, 2);
        image.SetPixel(1, 1, Color.Red);
        var source = image.GetHbitmap();
        try
        {
            using (ClipboardFormatHandle.Duplicate((ushort)format, source, TYMED.TYMED_GDI)) { }
            using var remaining = Image.FromHbitmap(source);
            Assert.Equal(image.Size, remaining.Size);
            Assert.Equal(Color.Red.ToArgb(), remaining.GetPixel(1, 1).ToArgb());
        }
        finally { DeleteObject(source); }
    }

    [Theory]
    [InlineData(3)]
    [InlineData(0x83)]
    public void MetafilePictureCopyOwnsAnIndependentNestedMetafile(int format)
    {
        var context = CreateMetaFile(null);
        Assert.NotEqual(0, context);
        Rectangle(context, 1, 2, 30, 40);
        var metafile = CloseMetaFile(context);
        Assert.NotEqual(0, metafile);
        var source = ClipboardNative.GlobalAlloc(0x42, (nuint)Marshal.SizeOf<MetafilePicture>());
        if (source == 0)
        {
            DeleteMetaFile(metafile);
            throw new InvalidOperationException("Cannot allocate test metafile picture.");
        }
        var medium = new STGMEDIUM { tymed = TYMED.TYMED_MFPICT, unionmember = source };
        try
        {
            var pointer = ClipboardNative.GlobalLock(source);
            Assert.NotEqual(0, pointer);
            try { Marshal.StructureToPtr(new MetafilePicture(8, 30, 40, metafile), pointer, false); }
            finally { ClipboardNative.GlobalUnlock(source); }

            var originalSize = GetMetaFileBitsEx(metafile, 0, 0);
            Assert.True(originalSize > 0);
            using (ClipboardFormatHandle.Duplicate((ushort)format, source, TYMED.TYMED_MFPICT)) { }
            Assert.Equal(originalSize, GetMetaFileBitsEx(metafile, 0, 0));
        }
        finally { ClipboardNative.ReleaseStgMedium(ref medium); }
    }

    [StructLayout(LayoutKind.Sequential)]
    private readonly struct MetafilePicture(int mappingMode, int width, int height, nint metafile)
    {
        private readonly int _mappingMode = mappingMode, _width = width, _height = height;
        private readonly nint _metafile = metafile;
    }

    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] private static extern nint CreateMetaFile(string? fileName);
    [DllImport("gdi32.dll")] private static extern nint CloseMetaFile(nint context);
    [DllImport("gdi32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool Rectangle(nint context, int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] private static extern uint GetMetaFileBitsEx(nint metafile, uint size, nint data);
    [DllImport("gdi32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool DeleteMetaFile(nint metafile);
    [DllImport("gdi32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool DeleteObject(nint value);

    private sealed class OpaqueNativeDataObject : DataObject, System.Windows.Forms.IDataObject, ComDataObject
    {
        private const string OpaqueFormat = "Speakeasy.UnitTest.OpaqueNative";
        private readonly bool _nativeAvailable;
        private readonly bool _emptyNativeHandle;
        public int NativeReads { get; private set; }

        internal OpaqueNativeDataObject(bool nativeAvailable, bool emptyNativeHandle = false)
        {
            _nativeAvailable = nativeAvailable;
            _emptyNativeHandle = emptyNativeHandle;
            SetData(DataFormats.UnicodeText, false, "clipboard seed");
        }

        public override string[] GetFormats(bool autoConvert) => [DataFormats.UnicodeText, OpaqueFormat];

        object? System.Windows.Forms.IDataObject.GetData(string format, bool autoConvert) =>
            format == OpaqueFormat ? null : "clipboard seed";

        void ComDataObject.GetData(ref FORMATETC descriptor, out STGMEDIUM medium)
        {
            NativeReads++;
            Assert.Equal(unchecked((ushort)DataFormats.GetFormat(OpaqueFormat).Id), unchecked((ushort)descriptor.cfFormat));
            Assert.Equal(TYMED.TYMED_HGLOBAL, descriptor.tymed);
            Assert.Equal(DVASPECT.DVASPECT_CONTENT, descriptor.dwAspect);
            Assert.Equal(-1, descriptor.lindex);
            if (!_nativeAvailable) throw new COMException("Native format unavailable.", unchecked((int)0x80040064));
            if (_emptyNativeHandle)
            {
                medium = new STGMEDIUM { tymed = TYMED.TYMED_HGLOBAL };
                return;
            }

            using var bytes = new MemoryStream([0, 255, 1, 0, 42]);
            var native = new DataObject();
            native.SetData(OpaqueFormat, false, bytes);
            ((ComDataObject)native).GetData(ref descriptor, out medium);
        }
    }

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
