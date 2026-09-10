using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace Speakeasy.App.Platform;

internal static class KeyboardInput
{
    // A process-local marker distinguishes our paste from remote-desktop and remapper
    // SendInput events. Windows' injected flag alone does not identify their source.
    internal static readonly nuint OwnEventMarker = (nuint)RandomNumberGenerator.GetInt32(1, int.MaxValue);

    internal static Input Key(ushort key, bool keyUp = false) => new()
    {
        Type = 1,
        Data = new InputUnion
        {
            Keyboard = new KeyInput { VirtualKey = key, Flags = keyUp ? 2u : 0u, ExtraInfo = OwnEventMarker }
        }
    };

    internal static Input Unicode(char codeUnit, bool keyUp = false) => new()
    {
        Type = 1,
        Data = new InputUnion
        {
            Keyboard = new KeyInput
            {
                ScanCode = codeUnit,
                Flags = 4u | (keyUp ? 2u : 0u),
                ExtraInfo = OwnEventMarker
            }
        }
    };

    internal static Input[] UnicodeScalar(string text, int offset)
    {
        ArgumentNullException.ThrowIfNull(text);
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(offset, text.Length);
        if (offset == text.Length) return [];
        var count = char.IsSurrogatePair(text, offset) ? 2 : 1;
        var inputs = new Input[count * 2];
        for (var index = 0; index < count; index++)
        {
            inputs[index * 2] = Unicode(text[offset + index]);
            inputs[index * 2 + 1] = Unicode(text[offset + index], keyUp: true);
        }
        return inputs;
    }

    [StructLayout(LayoutKind.Sequential)] internal struct Input { public uint Type; public InputUnion Data; }
    [StructLayout(LayoutKind.Explicit)]
    internal struct InputUnion
    {
        [FieldOffset(0)] public KeyInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }
    [StructLayout(LayoutKind.Sequential)] internal struct KeyInput { public ushort VirtualKey, ScanCode; public uint Flags, Time; public nuint ExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] internal struct MouseInput { public int X, Y; public uint Data, Flags, Time; public nuint ExtraInfo; }
}
