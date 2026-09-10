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
        Assert.True(recording.Append(MakeSignal(16_000 * 60 + 5_000)));
        Assert.True(recording.Append(minute));
        var result = recording.Finish();
        Assert.Equal(TimeSpan.FromMinutes(5), result.Duration);
        Assert.Equal(PcmRecording.MaximumBytes + 44, result.WaveData.Length);
    }

    [Theory]
    [InlineData(2, 0)]
    [InlineData(0, 3)]
    [InlineData(2, 3)]
    public void TrimsLongQuietEdgesWithHalfASecondOfPadding(int leadingSeconds, int trailingSeconds)
    {
        using var recording = new PcmRecording(0.003);
        var leading = new byte[leadingSeconds * PcmRecording.BytesPerSecond];
        var speech = MakeSignal(PcmRecording.SampleRate);
        var trailing = new byte[trailingSeconds * PcmRecording.BytesPerSecond];
        recording.Append(leading);
        recording.Append(speech);
        recording.Append(trailing);

        var result = recording.Finish();
        var expected = new byte[(leadingSeconds > 0 ? PcmRecording.BytesPerSecond / 2 : 0)]
            .Concat(speech)
            .Concat(new byte[(trailingSeconds > 0 ? PcmRecording.BytesPerSecond / 2 : 0)])
            .ToArray();
        Assert.True(result.HasSpeech);
        Assert.Equal(leading.Length + speech.Length + trailing.Length, recording.Length);
        Assert.Equal(expected, result.WaveData[44..]);
        Assert.Equal(TimeSpan.FromSeconds((double)expected.Length / PcmRecording.BytesPerSecond), result.Duration);
        Assert.Equal(expected.Length + 36, BinaryPrimitives.ReadInt32LittleEndian(result.WaveData.AsSpan(4)));
        Assert.Equal(expected.Length, BinaryPrimitives.ReadInt32LittleEndian(result.WaveData.AsSpan(40)));
    }

    [Fact]
    public void KeepsQuietPhonemesWithinPaddingAndEveryInteriorPauseByte()
    {
        using var recording = new PcmRecording(0.003);
        var quietPhoneme = MakeSignal(PcmRecording.SampleRate * 4 / 10, amplitude: 30);
        var speech = MakeSignal(PcmRecording.SampleRate * 3 / 10);
        var interiorPause = new byte[PcmRecording.BytesPerSecond * 2];
        var retained = quietPhoneme.Concat(speech).Concat(interiorPause).Concat(speech).Concat(quietPhoneme).ToArray();
        recording.Append(new byte[PcmRecording.BytesPerSecond * 2]);
        recording.Append(quietPhoneme);
        recording.Append(speech);
        recording.Append(interiorPause);
        recording.Append(speech);
        recording.Append(quietPhoneme);
        recording.Append(new byte[PcmRecording.BytesPerSecond * 2]);

        var result = recording.Finish();
        var expected = new byte[PcmRecording.BytesPerSecond / 10].Concat(retained)
            .Concat(new byte[PcmRecording.BytesPerSecond / 10]).ToArray();
        Assert.Equal(expected, result.WaveData[44..]);
        Assert.Equal(TimeSpan.FromMilliseconds(3600), result.Duration);
    }

    [Theory]
    [InlineData(980, 980)]
    [InlineData(1000, 500)]
    public void OnlyTrimsQuietEdgesOfAtLeastOneSecond(int edgeMilliseconds, int retainedMilliseconds)
    {
        using var recording = new PcmRecording(0.003);
        var edge = new byte[PcmRecording.BytesPerSecond * edgeMilliseconds / 1000];
        var speech = MakeSignal(PcmRecording.SampleRate);
        recording.Append(edge);
        recording.Append(speech);
        recording.Append(edge);

        var result = recording.Finish();
        var retainedEdge = new byte[PcmRecording.BytesPerSecond * retainedMilliseconds / 1000];
        Assert.Equal(retainedEdge.Concat(speech).Concat(retainedEdge).ToArray(), result.WaveData[44..]);
        Assert.Equal(TimeSpan.FromMilliseconds(1000 + retainedMilliseconds * 2), result.Duration);
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

    private static byte[] MakeSignal(int samples, int amplitude = 2000)
    {
        var data = new byte[samples * 2];
        for (var i = 0; i < samples; i++)
            BinaryPrimitives.WriteInt16LittleEndian(data.AsSpan(i * 2), (short)(amplitude * Math.Sin(i * 0.12)));
        return data;
    }
}
