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
    private const int EnergyWindowBytes = BytesPerSecond / 50;
    private const int MinimumQuietEdgeBytes = BytesPerSecond;
    private const int EdgePaddingBytes = BytesPerSecond / 2;
    private readonly MemoryStream _pcm = new(BytesPerSecond * 2);
    private int _audibleSamples;
    internal float Level { get; private set; }
    internal int Length => checked((int)_pcm.Length);

    internal bool Append(ReadOnlySpan<byte> data)
    {
        var bytes = Math.Min(data.Length & ~1, MaximumBytes - Length);
        var rms = CalculateRms(data[..bytes]);
        Level = (float)rms;
        if (rms >= silenceThreshold) _audibleSamples += bytes / 2;
        _pcm.Write(data[..bytes]);
        return Length >= MaximumBytes;
    }

    internal RecordedAudio Finish()
    {
        var hasSpeech = _audibleSamples >= SampleRate / 10;
        var pcm = GetTranscriptionPcm(hasSpeech);
        var result = new byte[44 + pcm.Length];
        using (var output = new MemoryStream(result))
        using (var writer = new BinaryWriter(output, Encoding.UTF8, leaveOpen: true))
        {
            writer.Write("RIFF"u8);
            writer.Write(36 + pcm.Length);
            writer.Write("WAVEfmt "u8);
            writer.Write(16);
            writer.Write((short)1);
            writer.Write((short)1);
            writer.Write(SampleRate);
            writer.Write(BytesPerSecond);
            writer.Write((short)2);
            writer.Write((short)16);
            writer.Write("data"u8);
            writer.Write(pcm.Length);
            writer.Write(pcm);
        }
        return new RecordedAudio(result, TimeSpan.FromSeconds((double)pcm.Length / BytesPerSecond), hasSpeech);
    }

    private ReadOnlySpan<byte> GetTranscriptionPcm(bool hasSpeech)
    {
        var pcm = _pcm.GetBuffer().AsSpan(0, Length);
        if (!hasSpeech) return pcm;

        var start = 0;
        while (start < pcm.Length)
        {
            var bytes = Math.Min(EnergyWindowBytes, pcm.Length - start);
            if (CalculateRms(pcm.Slice(start, bytes)) >= silenceThreshold) break;
            start += bytes;
        }
        if (start == pcm.Length) return pcm;

        var end = pcm.Length;
        while (end > start)
        {
            var bytes = Math.Min(EnergyWindowBytes, end - start);
            if (CalculateRms(pcm.Slice(end - bytes, bytes)) >= silenceThreshold) break;
            end -= bytes;
        }

        // Keep quiet word boundaries and all interior pauses; only long idle edges are removed.
        start = start >= MinimumQuietEdgeBytes ? start - EdgePaddingBytes : 0;
        end = pcm.Length - end >= MinimumQuietEdgeBytes ? end + EdgePaddingBytes : pcm.Length;
        return pcm[start..end];
    }

    private static double CalculateRms(ReadOnlySpan<byte> data)
    {
        double energy = 0;
        for (var offset = 0; offset < data.Length; offset += 2)
        {
            var amplitude = BinaryPrimitives.ReadInt16LittleEndian(data.Slice(offset, 2)) / 32768.0;
            energy += amplitude * amplitude;
        }
        return data.IsEmpty ? 0 : Math.Sqrt(energy / (data.Length / 2));
    }

    public void Dispose()
    {
        if (_pcm.TryGetBuffer(out var data)) data.AsSpan().Clear();
        _pcm.Dispose();
        Level = 0;
    }
}
