using System.Diagnostics;
using System.Text.Json;
using NAudio.Wave;
using Speakeasy.Core;
using Speakeasy.Core.Configuration;
using Speakeasy.Core.Transcription;

namespace Speakeasy.App.Diagnostics;

/// <summary>Explicit file-only diagnostic: never opens a microphone or touches the clipboard.</summary>
internal static class TranscriptionProbe
{
    public static async Task<int> RunAsync(string configDirectory, string audioFile, string resultFile)
    {
        var reports = new List<object>();
        try
        {
            var settings = new ConfigStore(configDirectory).Load();
            using var reader = new WaveFileReader(audioFile);
            if (reader.WaveFormat.SampleRate != 16000 || reader.WaveFormat.Channels != 1 || reader.WaveFormat.BitsPerSample != 16)
                throw new InvalidDataException("The diagnostic expects 16 kHz mono PCM16 WAV audio.");
            var audio = new RecordedAudio(await File.ReadAllBytesAsync(audioFile), reader.TotalTime, true);
            using var pipeline = new TranscriptionPipeline(settings, configDirectory);
            var clock = Stopwatch.StartNew();
            using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(5));
            await pipeline.WarmupAsync(timeout.Token);
            var warmup = clock.Elapsed.TotalMilliseconds;
            for (var attempt = 0; attempt < 3; attempt++)
            {
                clock.Restart();
                var stages = new List<object>();
                var result = await pipeline.TranscribeAsync(audio, stage => stages.Add(new { stage, milliseconds = clock.Elapsed.TotalMilliseconds }), timeout.Token);
                reports.Add(new { attempt = attempt + 1, milliseconds = clock.Elapsed.TotalMilliseconds, result.Text, result.Warning, stages });
            }
            await WriteResultAsync(resultFile, new
            {
                success = true,
                warmupMilliseconds = warmup,
                audioSeconds = audio.Duration.TotalSeconds,
                microphoneDevices = WaveInEvent.DeviceCount,
                reports
            });
            return 0;
        }
        catch (Exception exception)
        {
            await WriteResultAsync(resultFile, new { success = false, error = exception.Message, reports });
            return 1;
        }
    }

    private static async Task WriteResultAsync(string path, object result)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(result, new JsonSerializerOptions { WriteIndented = true }));
    }
}
