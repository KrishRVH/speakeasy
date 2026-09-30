# Remote dictation

Run Speakeasy on the host where the destination app is open. Remote keyboard
input and remote microphone audio are separate routes. Windows Speakeasy accepts injected
shortcut events and ignores its own insertion events, but the host must also have
a recording device receiving the client's voice.

The routing steps below describe a Windows host. Linux desktop portals and macOS
remote-input permissions need acceptance with the chosen remote client.

## Route the microphone

If your remote setup already exposes a host recording device, select it in
Speakeasy **Settings → Microphone**. Use **Refresh** after adding a device. Keep
desktop playback out of this input so the app receives only the intended voice.

For an existing Voicemeeter setup, a VBAN sender can send client microphone audio
to an incoming host stream. Match the destination address, stream name, and port,
then route that input strip to a dedicated virtual recording bus. Select that
bus in Speakeasy. Follow the vendor's [VBAN routing instructions](https://vb-audio.com/Voicemeeter/vban.htm).

With VB-CABLE, route a host receiver's playback into **CABLE Input** and select
**CABLE Output** as Speakeasy's microphone. These names describe opposite ends
of the same virtual cable; selecting the output alone does not feed it audio.
See the [VB-CABLE signal path](https://vb-audio.com/Cable/).

Use the receiver or Windows input meter to confirm that client speech reaches
the selected recording device. Then focus a scratch editor, hold
**Ctrl+Win**, speak, and release. This is a live dictation test and can
change the clipboard and editor text. Stop the client's microphone sender when
finished; Speakeasy's five-minute limit controls only its own recording.

## Troubleshooting

- **No pill:** confirm that dictation is enabled and the remote client forwards
  Ctrl+Win to the host. Many clients send Windows-key combinations only in full
  screen; check the client's keyboard setting. The shortcut is fixed.
- **Pill without a level:** check the sender, receiver, mixer route, and selected
  recording device in that order. Check host microphone permissions as well.
- **Level follows desktop sound:** separate playback from the microphone bus.
- **Recording continues after disconnect:** a streaming disconnect may leave the
  host session active. Reconnect and press Escape; the five-minute cap still
  bounds the recording.

Speakeasy does not install audio drivers or configure remote audio routes. The
complete client-to-host microphone path requires live acceptance with that setup;
mock capture and prerecorded inference do not verify it.
