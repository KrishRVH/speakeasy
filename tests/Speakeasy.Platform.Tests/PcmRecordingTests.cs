using System.Buffers.Binary;
using Speakeasy.App.Platform;

namespace Speakeasy.Platform.Tests;

public sealed class PcmRecordingTests
{
    [Fact]
    public void FinishesAStandard16KhzMonoPcmWaveAndRecognizesSpeechEnergy()
    {
        using var recording = new PcmRecording(0.003);
        var data = MakeSignal(16_000);
        recording.Append(data);
        var result = recording.Finish();
        Assert.True(result.HasSpeech);
        Assert.Equal(TimeSpan.FromSeconds(1), result.Duration);
        Assert.Equal(32_044, result.WaveData.Length);
        Assert.Equal("RIFF"u8.ToArray(), result.WaveData[..4]);
        Assert.Equal("WAVEfmt "u8.ToArray(), result.WaveData[8..16]);
        Assert.Equal(1, BinaryPrimitives.ReadInt16LittleEndian(result.WaveData.AsSpan(20)));
        Assert.Equal(1, BinaryPrimitives.ReadInt16LittleEndian(result.WaveData.AsSpan(22)));
        Assert.Equal(16_000, BinaryPrimitives.ReadInt32LittleEndian(result.WaveData.AsSpan(24)));
        Assert.Equal(16, BinaryPrimitives.ReadInt16LittleEndian(result.WaveData.AsSpan(34)));
        Assert.Equal(32_000, BinaryPrimitives.ReadInt32LittleEndian(result.WaveData.AsSpan(40)));
        Assert.Equal(data, result.WaveData[44..]);
    }

    [Fact]
    public void ClipsOversizedLastBufferAtFiveMinutes()
    {
        using var recording = new PcmRecording(0.003);
        var minute = MakeSignal(16_000 * 60);
        for (var i = 0; i < 4; i++) Assert.False(recording.Append(minute));
        Assert.True(recording.Append(new byte[minute.Length + 10_000]));
        Assert.True(recording.Append(minute));
        var result = recording.Finish();
        Assert.Equal(TimeSpan.FromMinutes(5), result.Duration);
        Assert.Equal(PcmRecording.MaximumBytes + 44, result.WaveData.Length);
    }

    [Fact]
    public void SilenceAndVeryShortClicksDoNotCountAsSpeech()
    {
        using var recording = new PcmRecording(0.003);
        recording.Append(new byte[32_000]);
        recording.Append(MakeSignal(160));
        var result = recording.Finish();
        Assert.False(result.HasSpeech);
        Assert.Equal(TimeSpan.FromMilliseconds(1010), result.Duration);
    }

    [Fact]
    public void DropsIncompletePcmSampleAtBufferBoundary()
    {
        using var recording = new PcmRecording(0.003);
        recording.Append([0, 0, 255]);
        Assert.Equal(2, recording.Length);
    }

    private static byte[] MakeSignal(int samples)
    {
        var data = new byte[samples * 2];
        for (var i = 0; i < samples; i++)
            BinaryPrimitives.WriteInt16LittleEndian(data.AsSpan(i * 2), (short)(2000 * Math.Sin(i * 0.12)));
        return data;
    }
}
