using System.Buffers.Binary;
using System.Text;
using Speakeasy.Core;

namespace Speakeasy.App.Platform;

/// <summary>Bounded 16 kHz / 16-bit / mono capture storage; contains no device APIs.</summary>
internal sealed class PcmRecording(double silenceThreshold) : IDisposable
{
    internal const int SampleRate = 16_000;
    internal const int BytesPerSecond = SampleRate * 2;
    internal const int MaximumBytes = BytesPerSecond * 300;
    private readonly MemoryStream _pcm = new(BytesPerSecond * 2);
    private int _audibleSamples;
    internal float Level { get; private set; }
    internal int Length => checked((int)_pcm.Length);

    internal bool Append(ReadOnlySpan<byte> data)
    {
        var bytes = Math.Min(data.Length & ~1, MaximumBytes - Length);
        var samples = bytes / 2;
        double energy = 0;
        for (var offset = 0; offset < bytes; offset += 2)
        {
            var amplitude = BinaryPrimitives.ReadInt16LittleEndian(data.Slice(offset, 2)) / 32768.0;
            energy += amplitude * amplitude;
        }
        var rms = samples == 0 ? 0 : Math.Sqrt(energy / samples);
        Level = (float)rms;
        if (rms >= silenceThreshold) _audibleSamples += samples;
        _pcm.Write(data[..bytes]);
        return Length >= MaximumBytes;
    }

    internal RecordedAudio Finish()
    {
        var result = new byte[44 + Length];
        using (var output = new MemoryStream(result))
        using (var writer = new BinaryWriter(output, Encoding.UTF8, leaveOpen: true))
        {
            writer.Write("RIFF"u8);
            writer.Write(36 + Length);
            writer.Write("WAVEfmt "u8);
            writer.Write(16);
            writer.Write((short)1);
            writer.Write((short)1);
            writer.Write(SampleRate);
            writer.Write(BytesPerSecond);
            writer.Write((short)2);
            writer.Write((short)16);
            writer.Write("data"u8);
            writer.Write(Length);
            _pcm.Position = 0;
            _pcm.CopyTo(output);
        }
        return new RecordedAudio(result, TimeSpan.FromSeconds((double)Length / BytesPerSecond),
            _audibleSamples >= SampleRate / 10);
    }

    public void Dispose()
    {
        if (_pcm.TryGetBuffer(out var data)) data.AsSpan().Clear();
        _pcm.Dispose();
        Level = 0;
    }
}
