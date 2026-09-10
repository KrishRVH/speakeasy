using Speakeasy.Core.Configuration;

namespace Speakeasy.Tests;

public sealed class ConfigurationTests : IDisposable
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "speakeasy-config-test-" + Guid.NewGuid().ToString("N"));

    [Fact]
    public void FirstLoadCreatesOfflineDefaultsAndKeyTemplate()
    {
        var store = new ConfigStore(_directory);
        var settings = store.Load();
        Assert.Equal("local", settings.Transcription.Provider);
        Assert.Equal("auto", settings.Cleanup.Provider);
        Assert.Equal(300, settings.MaxRecordingSeconds);
        Assert.False(settings.RestoreClipboard);
        Assert.Contains("\"hotkey\"", File.ReadAllText(store.SettingsPath));
        Assert.Contains("# OPENAI_API_KEY=", File.ReadAllText(store.EnvPath));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(301)]
    [InlineData(int.MaxValue)]
    public void RecordingLimitCannotExceedFiveMinutes(int seconds)
    {
        var settings = new AppSettings { MaxRecordingSeconds = seconds };
        Assert.Throws<ConfigurationException>(settings.Validate);
    }

    [Fact]
    public void SaveRoundTripsAndPreservesSecretsFile()
    {
        var store = new ConfigStore(_directory);
        var settings = store.Load();
        File.WriteAllText(store.EnvPath, "GROQ_API_KEY=fake-key\n");
        settings.Hotkey = "F9";
        settings.RestoreClipboard = true;
        store.Save(settings);
        var saved = store.Load();
        Assert.Equal("F9", saved.Hotkey);
        Assert.True(saved.RestoreClipboard);
        Assert.Equal("GROQ_API_KEY=fake-key\n", File.ReadAllText(store.EnvPath));
        Assert.Empty(Directory.GetFiles(_directory, "*.tmp"));
    }

    [Theory]
    [InlineData("{\"maxRecordingSecond\":300}")]
    [InlineData("{\"OPENAI_API_KEY\":\"do-not-echo-this\"}")]
    [InlineData("{bad-json-do-not-echo-this}")]
    public void InvalidSettingsAreRejectedWithoutEchoingContent(string json)
    {
        var store = new ConfigStore(_directory);
        store.Load();
        File.WriteAllText(store.SettingsPath, json);
        var error = Assert.Throws<ConfigurationException>(() => store.Load());
        Assert.DoesNotContain("do-not-echo-this", error.Message);
        Assert.Equal(json, File.ReadAllText(store.SettingsPath));
    }

    [Theory]
    [InlineData("http://example.com/v1")]
    [InlineData("https://example.com/v1")]
    [InlineData("http://localhost.attacker.test/v1")]
    [InlineData("file:///etc/passwd")]
    [InlineData("http://key@localhost:11434/v1")]
    [InlineData("http://localhost:11434/v1?key=secret")]
    public void LocalCleanupRejectsNonLoopbackAndEmbeddedCredentials(string endpoint)
    {
        var settings = new AppSettings { Cleanup = new() { Provider = "local", Model = "local-model", Endpoint = endpoint } };
        Assert.Throws<ConfigurationException>(settings.Validate);
    }

    [Theory]
    [InlineData("http://localhost:11434/v1")]
    [InlineData("http://127.0.0.1:1234/v1")]
    [InlineData("https://[::1]:1234/v1")]
    public void LocalCleanupAcceptsLoopback(string endpoint)
    {
        var settings = new AppSettings { Cleanup = new() { Provider = "local", Model = "local-model", Endpoint = endpoint } };
        settings.Validate();
    }

    [Fact]
    public void EnvironmentOverridesDotEnvWithoutMutatingProcessEnvironment()
    {
        Directory.CreateDirectory(_directory);
        var path = Path.Combine(_directory, ".env");
        File.WriteAllText(path, "# comment\nexport OPENAI_API_KEY=\"file-key\" # comment\nGROQ_API_KEY='groq-key'\nEMPTY=\nHASH=literal#hash # comment\n");
        var environment = new EnvironmentFile(path, name => name == "OPENAI_API_KEY" ? "environment-key" : null);
        Assert.Equal("environment-key", environment.Get("OPENAI_API_KEY"));
        Assert.Equal("groq-key", environment.Get("GROQ_API_KEY"));
        Assert.Equal("literal#hash", environment.Get("HASH"));
        Assert.Null(environment.Get("EMPTY"));
    }

    [Theory]
    [InlineData("bad-secret-line")]
    [InlineData(" =bad-secret-line")]
    [InlineData("1INVALID=bad-secret-line")]
    [InlineData("KEY=\"bad-secret-line")]
    public void DotEnvErrorsDoNotEchoSecrets(string source)
    {
        Directory.CreateDirectory(_directory);
        var path = Path.Combine(_directory, ".env");
        File.WriteAllText(path, source);
        var error = Assert.Throws<ConfigurationException>(() => new EnvironmentFile(path, _ => null));
        Assert.DoesNotContain("bad-secret-line", error.Message);
        Assert.Contains("line 1", error.Message);
    }

    public void Dispose()
    {
        if (Directory.Exists(_directory)) Directory.Delete(_directory, recursive: true);
    }
}
