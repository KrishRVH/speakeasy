using System.Text;
using Speakeasy.App.Platform;

namespace Speakeasy.Platform.Tests;

public sealed class KeyboardInputTests
{
    [Theory]
    [InlineData(0x0041)]
    [InlineData(0x00E9)]
    [InlineData(0x000D)]
    [InlineData(0x000A)]
    [InlineData(0xD83D)]
    [InlineData(0xDE00)]
    public void SendsMarkedUtf16PacketsWithoutPhysicalReturnOrOtherVirtualKeys(int codeUnit)
    {
        var down = KeyboardInput.Unicode((char)codeUnit);
        var up = KeyboardInput.Unicode((char)codeUnit, keyUp: true);

        Assert.Equal(1u, down.Type);
        Assert.Equal(1u, up.Type);
        Assert.Equal((ushort)codeUnit, down.Data.Keyboard.ScanCode);
        Assert.Equal((ushort)codeUnit, up.Data.Keyboard.ScanCode);
        Assert.Equal(0, down.Data.Keyboard.VirtualKey);
        Assert.Equal(0, up.Data.Keyboard.VirtualKey);
        Assert.Equal(4u, down.Data.Keyboard.Flags);
        Assert.Equal(6u, up.Data.Keyboard.Flags);
        Assert.NotEqual((nuint)0, KeyboardInput.OwnEventMarker);
        Assert.Equal(KeyboardInput.OwnEventMarker, down.Data.Keyboard.ExtraInfo);
        Assert.Equal(KeyboardInput.OwnEventMarker, up.Data.Keyboard.ExtraInfo);
    }

    [Fact]
    public void ScalarsReconstructTextWithoutSplittingSurrogatePairs()
    {
        string[] scalars = ["A", "\U0001F600", "\r", "\n", "é", "\uD83D", "!", "\uDE00"];
        var text = string.Concat(scalars);
        var reconstructed = new StringBuilder();
        var offset = 0;
        foreach (var scalar in scalars)
        {
            var inputs = KeyboardInput.UnicodeScalar(text, offset);
            Assert.Equal(scalar.Length * 2, inputs.Length);
            for (var index = 0; index < inputs.Length; index += 2)
            {
                var down = inputs[index].Data.Keyboard;
                var up = inputs[index + 1].Data.Keyboard;
                Assert.Equal((ushort)scalar[index / 2], down.ScanCode);
                Assert.Equal(down.ScanCode, up.ScanCode);
                Assert.Equal(4u, down.Flags);
                Assert.Equal(6u, up.Flags);
                Assert.Equal(KeyboardInput.OwnEventMarker, down.ExtraInfo);
                Assert.Equal(KeyboardInput.OwnEventMarker, up.ExtraInfo);
                reconstructed.Append((char)down.ScanCode);
            }
            offset += inputs.Length / 2;
        }
        Assert.Equal(text.Length, offset);
        Assert.Equal(text, reconstructed.ToString());
        Assert.Empty(KeyboardInput.UnicodeScalar(text, text.Length));
        Assert.Empty(KeyboardInput.UnicodeScalar("", 0));
    }
}
