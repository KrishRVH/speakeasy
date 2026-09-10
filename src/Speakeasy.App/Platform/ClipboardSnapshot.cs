using System.Collections.Specialized;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;
using ComDataObject = System.Runtime.InteropServices.ComTypes.IDataObject;

namespace Speakeasy.App.Platform;

/// <summary>
/// Materializes every advertised IDataObject format. Unsupported formats abort capture
/// before the clipboard is modified; restoration never silently degrades to text-only.
/// </summary>
internal sealed class ClipboardSnapshot : IDisposable
{
    private readonly List<ClipboardFormatHandle> _formats = [];
    internal int FormatCount => _formats.Count;

    internal static ClipboardSnapshot Capture() => CaptureFrom(Clipboard.GetDataObject(), CaptureNativeFormat);

    internal static ClipboardSnapshot CaptureFrom(System.Windows.Forms.IDataObject? source, Func<ushort, ClipboardFormatHandle>? nativeFallback = null)
    {
        var snapshot = new ClipboardSnapshot();
        var disposables = new List<IDisposable>();
        try
        {
            if (source == null) return snapshot;
            var materialized = new DataObject();
            var nativeSource = source as ComDataObject;
            var nativeFormats = new HashSet<string>();
            var formats = source.GetFormats(autoConvert: false);
            foreach (var format in formats)
            {
                var original = source.GetData(format, autoConvert: false);
                if (original == null)
                {
                    // Windows apps can advertise opaque formats that have no managed
                    // conversion. Preserve their native bytes instead of dropping them.
                    if (nativeSource == null)
                        throw new NotSupportedException($"Clipboard format '{format}' cannot be saved without data loss. The original clipboard was preserved.");
                    nativeFormats.Add(format);
                    continue;
                }
                var value = CloneValue(original);
                if (value is IDisposable disposable) disposables.Add(disposable);
                materialized.SetData(format, autoConvert: false, value);
            }
            // Turn the detached IDataObject into native clipboard handles up front. The
            // expensive work and any unsupported-format failure precede EmptyClipboard.
            foreach (var format in formats)
            {
                var id = unchecked((ushort)DataFormats.GetFormat(format).Id);
                var descriptor = new FORMATETC
                {
                    cfFormat = unchecked((short)id),
                    dwAspect = DVASPECT.DVASPECT_CONTENT,
                    lindex = -1,
                    tymed = id switch
                    {
                        2 or 9 or 0x82 => TYMED.TYMED_GDI,
                        14 or 0x8E => TYMED.TYMED_ENHMF,
                        3 or 0x83 => TYMED.TYMED_MFPICT,
                        _ => TYMED.TYMED_HGLOBAL
                    }
                };
                var native = nativeFormats.Contains(format) ? nativeSource! : (ComDataObject)materialized;
                native.GetData(ref descriptor, out var medium);
                try
                {
                    var copy = medium.tymed == TYMED.TYMED_HGLOBAL && medium.unionmember == 0 && nativeFallback != null
                        ? nativeFallback(id)
                        : ClipboardFormatHandle.Duplicate(id, medium.unionmember, medium.tymed);
                    snapshot._formats.Add(copy);
                }
                finally { ClipboardNative.ReleaseStgMedium(ref medium); }
            }
            GC.KeepAlive(materialized);
            return snapshot;
        }
        catch
        {
            snapshot.Dispose();
            throw;
        }
        finally
        {
            foreach (var disposable in disposables) disposable.Dispose();
        }
    }

    private static ClipboardFormatHandle CaptureNativeFormat(ushort format)
    {
        var sequence = ClipboardNative.GetClipboardSequenceNumber();
        if (!ClipboardNative.OpenClipboard(0))
            throw new ExternalException("Another application is holding the clipboard open. The original clipboard was preserved; try again.");
        try
        {
            if (ClipboardNative.GetClipboardSequenceNumber() != sequence)
                throw new ExternalException("The clipboard changed while it was being saved. Its new contents were preserved; try again.");
            var borrowed = ClipboardNative.GetClipboardData(format);
            if (borrowed == 0)
            {
                var error = Marshal.GetLastWin32Error();
                throw new ExternalException($"Clipboard format '{DataFormats.GetFormat(format).Name}' could not supply its native data (Windows error {error}). The original clipboard was preserved.");
            }
            // GetClipboardData lends this handle only while the clipboard is open.
            // Copy it before closing; the original remains owned by Windows.
            return ClipboardFormatHandle.Duplicate(format, borrowed, TYMED.TYMED_HGLOBAL);
        }
        finally { ClipboardNative.CloseClipboard(); }
    }

    private static object CloneValue(object value) => value switch
    {
        string text => text,
        byte[] data => data.Clone(),
        string[] paths => paths.Clone(),
        StringCollection strings => CloneStrings(strings),
        Image image => image.Clone(),
        Stream stream => CloneStream(stream),
        _ when value.GetType().IsPrimitive || value is decimal or DateTime or Guid => value,
        _ => throw new NotSupportedException($"Clipboard data of type '{value.GetType().Name}' cannot be safely restored. The original clipboard was preserved.")
    };

    private static StringCollection CloneStrings(StringCollection source)
    {
        var result = new StringCollection();
        foreach (string? text in source) result.Add(text);
        return result;
    }

    private static MemoryStream CloneStream(Stream stream)
    {
        if (!stream.CanSeek)
            throw new NotSupportedException("A clipboard stream cannot be saved without consuming it. The original clipboard was preserved.");
        var position = stream.Position;
        var result = new MemoryStream();
        try
        {
            stream.Position = 0;
            stream.CopyTo(result);
            result.Position = 0;
            return result;
        }
        catch { result.Dispose(); throw; }
        finally { stream.Position = position; }
    }

    internal bool RestoreWhileOpen()
    {
        if (!ClipboardNative.EmptyClipboard()) return false;
        var success = true;
        foreach (var format in _formats) success &= format.TransferToClipboard();
        return success;
    }

    public void Dispose()
    {
        foreach (var format in _formats) format.Dispose();
        _formats.Clear();
    }
}

internal sealed class ClipboardFormatHandle(ushort format, nint handle, TYMED medium) : IDisposable
{
    private nint _handle = handle;

    internal static ClipboardFormatHandle FromText(string text)
    {
        var bytes = Encoding.Unicode.GetBytes(text + '\0');
        var handle = ClipboardNative.GlobalAlloc(0x42, checked((nuint)bytes.Length));
        if (handle == 0) throw new ExternalException("Windows could not allocate clipboard memory.");
        var target = ClipboardNative.GlobalLock(handle);
        if (target == 0)
        {
            ClipboardNative.GlobalFree(handle);
            throw new ExternalException("Windows could not access clipboard memory.");
        }
        try { Marshal.Copy(bytes, 0, target, bytes.Length); }
        finally { ClipboardNative.GlobalUnlock(handle); }
        return new ClipboardFormatHandle(13, handle, TYMED.TYMED_HGLOBAL);
    }

    internal static ClipboardFormatHandle Duplicate(ushort format, nint source, TYMED medium)
    {
        if (source == 0 || medium is not (TYMED.TYMED_HGLOBAL or TYMED.TYMED_GDI or TYMED.TYMED_ENHMF or TYMED.TYMED_MFPICT))
            throw new NotSupportedException($"Clipboard format '{DataFormats.GetFormat(format).Name}' ({medium}, {(source == 0 ? "empty handle" : "unsupported medium")}) cannot be independently saved. The original clipboard was preserved.");
        // Display formats retain their original ID, but contain the same native objects
        // as the corresponding ordinary formats that OleDuplicateData understands.
        var copyFormat = format switch { 0x82 => (ushort)2, 0x83 => (ushort)3, _ => format };
        var copy = medium == TYMED.TYMED_ENHMF
            ? ClipboardNative.CopyEnhMetaFile(source, null)
            : ClipboardNative.OleDuplicateData(source, copyFormat, 0);
        if (copy == 0) throw new ExternalException("Windows could not save every clipboard format. The original clipboard was preserved.");
        return new ClipboardFormatHandle(format, copy, medium);
    }

    internal bool TransferToClipboard()
    {
        if (_handle == 0) return false;
        if (ClipboardNative.SetClipboardData(format, _handle) == 0) return false;
        _handle = 0; // Ownership now belongs to Windows.
        return true;
    }

    public void Dispose()
    {
        if (_handle == 0) return;
        var value = new STGMEDIUM { tymed = medium, unionmember = _handle };
        ClipboardNative.ReleaseStgMedium(ref value);
        _handle = 0;
    }
}

internal static class ClipboardNative
{
    [DllImport("user32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] internal static extern bool OpenClipboard(nint owner);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] internal static extern bool CloseClipboard();
    [DllImport("user32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] internal static extern bool EmptyClipboard();
    [DllImport("user32.dll", SetLastError = true)] internal static extern nint SetClipboardData(uint format, nint memory);
    [DllImport("user32.dll", SetLastError = true)] internal static extern nint GetClipboardData(uint format);
    [DllImport("user32.dll")] internal static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll")] internal static extern nint GetClipboardOwner();
    [DllImport("kernel32.dll")] internal static extern nint GlobalAlloc(uint flags, nuint bytes);
    [DllImport("kernel32.dll")] internal static extern nint GlobalFree(nint memory);
    [DllImport("kernel32.dll")] internal static extern nint GlobalLock(nint memory);
    [DllImport("kernel32.dll")][return: MarshalAs(UnmanagedType.Bool)] internal static extern bool GlobalUnlock(nint memory);
    [DllImport("ole32.dll")] internal static extern nint OleDuplicateData(nint source, ushort format, uint flags);
    [DllImport("ole32.dll")] internal static extern void ReleaseStgMedium(ref STGMEDIUM medium);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] internal static extern nint CopyEnhMetaFile(nint source, string? file);
}
