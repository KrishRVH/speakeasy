using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Speakeasy.Core.Configuration;

public sealed class ConfigStore
{
    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        WriteIndented = true,
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow
    };

    private readonly string _directory;
    public string SettingsPath { get; }
    public string EnvPath { get; }

    public ConfigStore(string directory)
    {
        _directory = Path.GetFullPath(directory);
        SettingsPath = Path.Combine(_directory, "settings.json");
        EnvPath = Path.Combine(_directory, ".env");
    }

    public AppSettings Load()
    {
        Directory.CreateDirectory(_directory);
        if (!File.Exists(EnvPath))
        {
            // CreateNew avoids overwriting keys if another instance creates the file first.
            try
            {
                using var file = new FileStream(EnvPath, FileMode.CreateNew, FileAccess.Write);
                using var writer = new StreamWriter(file, new UTF8Encoding(false));
                writer.Write("# Optional provider keys. Process environment takes precedence.\n# OPENAI_API_KEY=\n# GROQ_API_KEY=\n# LOCAL_LLM_API_KEY=\n");
            }
            catch (IOException) when (File.Exists(EnvPath)) { }
        }
        if (!File.Exists(SettingsPath))
        {
            var defaults = new AppSettings();
            Save(defaults);
            return defaults;
        }
        try
        {
            var settings = JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(SettingsPath), JsonOptions)
                ?? throw new ConfigurationException("settings.json must contain a JSON object.");
            settings.Validate();
            return settings;
        }
        catch (JsonException exception)
        {
            // Never include source text: users sometimes accidentally put keys in settings.json.
            throw new ConfigurationException($"settings.json contains invalid JSON or an unknown setting near line {(exception.LineNumber ?? 0) + 1}. Check settings.example.json.");
        }
    }

    public void Save(AppSettings settings)
    {
        ArgumentNullException.ThrowIfNull(settings);
        settings.Validate();
        Directory.CreateDirectory(_directory);
        var temporaryPath = Path.Combine(_directory, $".settings-{Guid.NewGuid():N}.tmp");
        try
        {
            File.WriteAllText(temporaryPath, JsonSerializer.Serialize(settings, JsonOptions) + "\n", new UTF8Encoding(false));
            File.Move(temporaryPath, SettingsPath, overwrite: true);
        }
        finally
        {
            if (File.Exists(temporaryPath))
                File.Delete(temporaryPath);
        }
    }
}
