# Dictation through Moonlight and Sunshine

UI names and microphone-check steps here refer to the .NET app. The same host
audio routing applies to Rust; choose the receiving microphone in Rust Settings.

Run Speakeasy on the Windows host where your destination app is open. Sunshine
can deliver the shortcut and Escape through injected Windows keyboard events;
Speakeasy accepts those events and filters its own generated paste keystrokes.
Sunshine's Windows input path uses
[SendInput](https://github.com/LizardByte/Sunshine/blob/v2025.923.33222/src/platform/windows/input.cpp).

Client microphone audio needs a separate path. The stock Moonlight client does
not provide an installed microphone-forwarding feature; its
[upstream implementation is still an open pull request](https://github.com/moonlight-stream/moonlight-qt/pull/1648)
as checked on 2026-09-09. Sunshine's `audio_sink` and `virtual_sink` settings
capture host playback for the stream. They do not create an incoming client
microphone. See [Sunshine's audio configuration](https://docs.lizardbyte.dev/projects/sunshine/master/md_docs_2configuration.html#audio_sink).

## Use the existing Voicemeeter route

For a host that already has Voicemeeter Banana, receive the remote microphone
with VBAN and send it to a virtual recording bus. VB-Audio supplies a
[VBAN Talkie sender for Windows, macOS, Android, and iOS](https://vb-audio.com/Voicemeeter/vban.htm);
Voicemeeter receives VBAN streams on its input strips.

1. On the client, select the microphone in a VBAN sender. Set its destination to
   the Windows host's reachable LAN or private VPN address and choose a stream
   name, such as `SpeakeasyMic`.
2. On the host, open Voicemeeter's **VBAN** panel. Enable VBAN and an unused
   incoming stream. Match the sender's name, source address, and UDP port;
   the usual VBAN port is **6980**. Route it to an unused input strip.
3. Send that strip to a dedicated virtual bus, such as **B1**. Keep other audio
   out of that bus so dictation receives your voice without desktop playback.
   Preserve any existing mixer routes you use. Voicemeeter's
   [routing manual](https://vb-audio.com/Voicemeeter/VoicemeeterBanana_UserManual.pdf)
   explains the input strips and output buses.
4. In Speakeasy Preferences, choose **Voicemeeter Out B1** as the microphone.
   If you used B2, choose **Voicemeeter Out B2** instead.
5. Use **Check microphone** and speak at the client. The meter must respond to
   your voice before you try dictation. This check discards its audio.

VBAN is a separate audio connection. Use your LAN or a private VPN that can
carry it; opening Sunshine's ports does not forward this microphone stream.
Stop the client sender when finished. Speakeasy's five-minute limit bounds its
own recordings and does not stop another application's microphone sender.

## Alternative: receive into VB-CABLE

With VB-CABLE already installed, a standalone
[VBAN Receptor](https://vb-audio.com/Voicemeeter/vban.htm) can receive the same
client stream. Choose **CABLE Input** as the receiver's playback output, then
**CABLE Output** as Speakeasy's recording input. VB-CABLE deliberately uses
these names: audio played into its input is available at its output, as shown
in the [vendor's cable description](https://vb-audio.com/Cable/).

Keep Sunshine's playback capture separate from this cable. Selecting a cable
in Speakeasy alone supplies no audio; a receiver must feed it.

## If it does not respond

- No pill: the shortcut is not reaching the host. Try a simple key such as F8,
  confirm the Moonlight window captures keyboard input, and check that
  dictation is enabled.
- Pill but no microphone level: check the sender, incoming VBAN meter, mixer
  routing, and selected Windows recording device in that order.
- Level responds to game or desktop sound: the selected bus is carrying
  playback. Route only the client microphone into the dictation bus.
- It works while connected but continues after Moonlight closes: the Windows
  desktop may still be logged in. Reconnect and cancel, or let the configured
  recording limit finish; a streaming disconnect is not necessarily a Windows
  session disconnect.

Remote keyboard support is covered by automated tests. An actual client
microphone and receiver must be connected to verify the full audio path; the
app does not install audio drivers or change microphone privacy settings.
