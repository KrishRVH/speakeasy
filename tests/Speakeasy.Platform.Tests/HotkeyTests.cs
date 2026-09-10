using Speakeasy.App.Platform;
using Speakeasy.Core.Interaction;

namespace Speakeasy.Platform.Tests;

public sealed class HotkeyTests
{
    private const int Space = 0x20, LeftCtrl = 0xA2, RightCtrl = 0xA3, LeftAlt = 0xA4, RightAlt = 0xA5;

    [Fact]
    public void ChordSuppressesOnlyActivationAndItsRepeats()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Alt+Space"));
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftCtrl, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftAlt, true, false, true));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process('A', true, false, true));
        Assert.Equal(HotkeySignal.Up | HotkeySignal.Suppress, tracker.Process(Space, false, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftAlt, false, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftCtrl, false, false, true));
    }

    [Fact]
    public void ModifierReleaseEndsHoldAndFinalPrimaryReleaseStillSuppressed()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Alt+Space"));
        tracker.Process(LeftCtrl, true, false, true);
        tracker.Process(LeftAlt, true, false, true);
        tracker.Process(Space, true, false, true);
        Assert.Equal(HotkeySignal.Up, tracker.Process(LeftCtrl, false, false, true));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(Space, false, false, true));
        tracker.Process(LeftCtrl, true, false, true);
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
    }

    [Fact]
    public void KeyPressedBeforeModifiersNeverActivatesFromAutoRepeat()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Space"));
        Assert.Equal(HotkeySignal.None, tracker.Process(Space, true, false, true));
        tracker.Process(LeftCtrl, true, false, true);
        Assert.Equal(HotkeySignal.None, tracker.Process(Space, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(Space, false, false, true));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
    }

    [Fact]
    public void EscapeIsPassiveAndOnlyFiresOncePerPhysicalPress()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("F13"));
        Assert.Equal(HotkeySignal.Escape, tracker.Process(0x1B, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x1B, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x1B, false, false, true));
    }

    [Fact]
    public void InjectedInputDoesNotActivateCancelOrCorruptPhysicalState()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("RightAlt"));
        Assert.Equal(HotkeySignal.None, tracker.Process(RightAlt, true, true, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x1B, true, true, true));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(RightAlt, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(RightAlt, false, true, true));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(RightAlt, true, false, true));
        Assert.Equal(HotkeySignal.Up | HotkeySignal.Suppress, tracker.Process(RightAlt, false, false, true));
    }

    [Fact]
    public void SideSpecificModifiersRequireThatSide()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("RightCtrl+Space"));
        tracker.Process(LeftCtrl, true, false, true);
        Assert.Equal(HotkeySignal.None, tracker.Process(Space, true, false, true));
        tracker.Process(Space, false, false, true);
        tracker.Process(RightCtrl, true, false, true);
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftCtrl, false, false, true));
        Assert.Equal(HotkeySignal.Up, tracker.Process(RightCtrl, false, false, true));
    }

    [Fact]
    public void EitherModifierMayRemainHeldForGenericModifier()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Space"));
        tracker.Process(LeftCtrl, true, false, true);
        tracker.Process(RightCtrl, true, false, true);
        tracker.Process(Space, true, false, true);
        Assert.Equal(HotkeySignal.None, tracker.Process(LeftCtrl, false, false, true));
        Assert.Equal(HotkeySignal.Up, tracker.Process(RightCtrl, false, false, true));
    }

    [Fact]
    public void DisabledModeHasNoSuppressionOrEventsAndDoesNotReviveAHeldKey()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("F8"));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x77, true, false, false));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x1B, true, false, false));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x77, true, false, true));
        Assert.Equal(HotkeySignal.None, tracker.Process(0x77, false, false, true));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(0x77, true, false, true));
        Assert.Equal(HotkeySignal.Up | HotkeySignal.Suppress, tracker.Process(0x77, false, false, false));
    }

    [Fact]
    public void ReconciliationPreservesSuppressionForTheOriginalStillHeldKey()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("F8"));
        tracker.Process(0x77, true, false, true);
        tracker.ReconcilePressedKeys(key => key == 0x77);
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(0x77, true, false, true));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(0x77, false, false, true));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(0x77, true, false, true));
    }

    [Fact]
    public void ReleaseDuringPauseClearsGestureGuardForTheFirstPressAfterResume()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("F8"));
        var gesture = NewGesture();
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(0x77, true, false, true));
        gesture.KeyDown(TimeSpan.Zero);
        gesture.Cancel();
        Assert.Equal(HotkeySignal.Up | HotkeySignal.Suppress, tracker.Process(0x77, false, false, false));
        gesture.KeyUp(TimeSpan.FromMilliseconds(10));
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(0x77, true, false, true));
        Assert.Contains(GestureAction.StartRecording, gesture.KeyDown(TimeSpan.FromMilliseconds(20)));
    }

    [Fact]
    public void ModifierReleaseDuringPauseAlsoCompletesOutstandingActivation()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Space"));
        tracker.Process(LeftCtrl, true, false, true);
        tracker.Process(Space, true, false, true);
        Assert.Equal(HotkeySignal.Up, tracker.Process(LeftCtrl, false, false, false));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(Space, true, false, false));
        Assert.Equal(HotkeySignal.Suppress, tracker.Process(Space, false, false, false));
        tracker.Process(LeftCtrl, true, false, true);
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
    }

    [Fact]
    public void ReconciliationRecoversFirstPressWhenReleaseWasLostDuringLock()
    {
        var tracker = new HotkeyTracker(HotkeyGesture.Parse("Ctrl+Space"));
        var gesture = NewGesture();
        tracker.Process(LeftCtrl, true, false, true);
        tracker.Process(Space, true, false, true);
        gesture.KeyDown(TimeSpan.Zero);
        gesture.Cancel();
        // No keyup notifications arrive on this desktop while it is locked.
        tracker.ReconcilePressedKeys(_ => false);
        gesture.KeyUp(TimeSpan.FromSeconds(1)); // The hook posts this cleanup after reseeding.
        Assert.Equal(HotkeySignal.None, tracker.Process(Space, true, false, true));
        tracker.Process(Space, false, false, true);
        tracker.Process(LeftCtrl, true, false, true);
        Assert.Equal(HotkeySignal.Down | HotkeySignal.Suppress, tracker.Process(Space, true, false, true));
        Assert.Contains(GestureAction.StartRecording, gesture.KeyDown(TimeSpan.FromSeconds(2)));
    }

    private static DictationGesture NewGesture() => new(TimeSpan.FromMilliseconds(300), TimeSpan.FromMilliseconds(220), TimeSpan.FromMinutes(5));

    [Theory]
    [InlineData("F1", 0x70)]
    [InlineData("F24", 0x87)]
    [InlineData(" RightAlt ", RightAlt)]
    [InlineData("lctrl+RAlt", RightAlt)]
    [InlineData("Win+Shift+D", 0x44)]
    public void ParsesSupportedKeys(string shortcut, int expectedKey) => Assert.Equal(expectedKey, HotkeyGesture.Parse(shortcut).Key);

    [Theory]
    [InlineData("Fn")]
    [InlineData("Ctrl+Fn")]
    [InlineData("F25")]
    [InlineData("Escape")]
    [InlineData("Ctrl+Escape")]
    [InlineData("Ctrl+")]
    [InlineData("Control")]
    [InlineData("Ctrl+RightCtrl")]
    [InlineData("Ctrl+Ctrl+Ctrl+Ctrl+Ctrl+Space")]
    public void RejectsUnobservableReservedOrInvalidKeys(string shortcut) => Assert.Throws<ArgumentException>(() => HotkeyGesture.Parse(shortcut));
}
