namespace Speakeasy.App.Platform;

/// <summary>A Windows key, optionally qualified by left/right-specific modifiers.</summary>
public sealed class HotkeyGesture
{
    private readonly int[][] _modifiers;

    private HotkeyGesture(int key, int[][] modifiers)
    {
        Key = key;
        _modifiers = modifiers;
    }

    public int Key { get; }

    public static HotkeyGesture Parse(string shortcut)
    {
        if (string.IsNullOrWhiteSpace(shortcut))
            throw new ArgumentException("Choose a hotkey such as Ctrl+Alt+Space, F8, or RightAlt.");

        var parts = shortcut.Split('+', StringSplitOptions.TrimEntries);
        if (parts.Length > 5)
            throw new ArgumentException("A hotkey can contain at most four modifiers and one activation key.");
        if (parts.Any(p => p.Equals("Fn", StringComparison.OrdinalIgnoreCase)))
            throw new ArgumentException("Fn is usually handled by keyboard firmware and Windows cannot observe it. Remap Fn to F13 with your keyboard software, then choose F13, or use Ctrl+Alt+Space.");

        var modifiers = new List<int[]>();
        for (var i = 0; i < parts.Length - 1; i++)
            modifiers.Add(ParseModifier(parts[i]) ?? throw new ArgumentException($"Unknown modifier '{parts[i]}'. Use Ctrl, Alt, Shift, Win, or their Left/Right variants."));

        var primary = parts[^1];
        var key = ParseKey(primary);
        if (key == 0 || key == 0x1B)
            throw new ArgumentException($"'{primary}' is not a supported activation key. Escape is reserved for passive cancellation. Use Ctrl+Alt+Space, F1–F24, or a side-specific key such as RightAlt.");
        if (modifiers.Any(group => group.Contains(key)))
            throw new ArgumentException("The activation key cannot also be a required modifier.");
        return new HotkeyGesture(key, modifiers.ToArray());
    }

    internal bool Matches(bool[] down)
    {
        foreach (var group in _modifiers)
        {
            var anyDown = false;
            foreach (var key in group)
                anyDown |= down[key];
            if (!anyDown) return false;
        }
        return true;
    }

    private static int[]? ParseModifier(string token) => token.ToLowerInvariant() switch
    {
        "ctrl" or "control" => [0xA2, 0xA3],
        "alt" => [0xA4, 0xA5],
        "shift" => [0xA0, 0xA1],
        "win" or "windows" => [0x5B, 0x5C],
        "lctrl" or "leftctrl" or "lcontrol" or "leftcontrol" => [0xA2],
        "rctrl" or "rightctrl" or "rcontrol" or "rightcontrol" => [0xA3],
        "lalt" or "leftalt" => [0xA4],
        "ralt" or "rightalt" => [0xA5],
        "lshift" or "leftshift" => [0xA0],
        "rshift" or "rightshift" => [0xA1],
        "lwin" or "leftwin" or "leftwindows" => [0x5B],
        "rwin" or "rightwin" or "rightwindows" => [0x5C],
        _ => null
    };

    private static int ParseKey(string token)
    {
        var modifier = ParseModifier(token);
        if (modifier is { Length: 1 }) return modifier[0];
        if (token.Length == 1 && char.IsAsciiLetterOrDigit(token[0]))
            return char.ToUpperInvariant(token[0]);
        if (token.StartsWith('F') || token.StartsWith('f'))
        {
            if (int.TryParse(token.AsSpan(1), out var number) && number is >= 1 and <= 24)
                return 0x70 + number - 1;
        }
        return token.ToLowerInvariant() switch
        {
            "space" or "spacebar" => 0x20,
            "pause" => 0x13,
            "capslock" => 0x14,
            "insert" => 0x2D,
            "scrolllock" => 0x91,
            "tab" => 0x09,
            "enter" or "return" => 0x0D,
            "backspace" => 0x08,
            "delete" => 0x2E,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" => 0x21,
            "pagedown" => 0x22,
            "up" => 0x26,
            "down" => 0x28,
            "left" => 0x25,
            "right" => 0x27,
            _ => 0
        };
    }
}

[Flags]
internal enum HotkeySignal { None = 0, Suppress = 1, Down = 2, Up = 4, Escape = 8 }

/// <summary>Pure hook state machine. Only the hook thread mutates it.</summary>
internal sealed class HotkeyTracker(HotkeyGesture gesture)
{
    private readonly bool[] _down = new bool[256];
    private readonly nuint _ownInputMarker = KeyboardInput.OwnEventMarker;
    private bool _active;
    private bool _suppressedPrimary;

    internal void ReconcilePressedKeys(Func<int, bool> isPressed)
    {
        for (var key = 0; key < _down.Length; key++) _down[key] = isPressed(key);
        _active = false;
        // A lost release clears old ownership. If the original swallowed key is still
        // physically down, continue swallowing its repeats and matching release.
        _suppressedPrimary &= _down[gesture.Key];
    }

    internal HotkeySignal Process(int key, bool down, bool injected, bool enabled, nuint extraInfo = 0)
    {
        if ((injected && extraInfo == _ownInputMarker) || key is < 0 or >= 256) return HotkeySignal.None;
        var repeated = down && _down[key];
        _down[key] = down;

        if (key == 0x1B)
            return enabled && down && !repeated ? HotkeySignal.Escape : HotkeySignal.None;

        var signal = HotkeySignal.None;
        if (key == gesture.Key)
        {
            if (down)
            {
                if (_suppressedPrimary) return HotkeySignal.Suppress;
                if (enabled && !repeated && gesture.Matches(_down))
                {
                    _active = _suppressedPrimary = true;
                    return HotkeySignal.Down | HotkeySignal.Suppress;
                }
            }
            else if (_suppressedPrimary)
            {
                signal = HotkeySignal.Suppress;
                _suppressedPrimary = false;
                if (_active) signal |= HotkeySignal.Up;
                _active = false;
            }
        }

        // Disabled mode cannot begin an activation. It still finishes an activation
        // begun while enabled, including its suppression and the consumer's Up edge.
        // End hold-to-talk as soon as a required modifier is released. The activation
        // key's eventual keyup still gets swallowed, matching its swallowed keydown.
        if (_active && !gesture.Matches(_down))
        {
            _active = false;
            signal |= HotkeySignal.Up;
        }
        return signal;
    }
}
