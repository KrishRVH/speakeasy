namespace Speakeasy.Core.Configuration;

internal sealed class EnvironmentFile
{
    private readonly Dictionary<string, string> _values = new(StringComparer.Ordinal);
    private readonly Func<string, string?> _environment;

    internal EnvironmentFile(string path, Func<string, string?>? environment = null)
    {
        _environment = environment ?? Environment.GetEnvironmentVariable;
        if (!File.Exists(path)) return;
        var number = 0;
        foreach (var source in File.ReadLines(path))
        {
            number++;
            var line = source.Trim();
            if (line.Length == 0 || line.StartsWith('#')) continue;
            if (line.StartsWith("export ", StringComparison.Ordinal)) line = line[7..].TrimStart();
            var equals = line.IndexOf('=');
            if (equals <= 0) throw InvalidLine(number);
            var name = line[..equals].Trim();
            if (name.Length == 0 || !name.All(c => char.IsAsciiLetterOrDigit(c) || c == '_') || char.IsDigit(name[0]))
                throw InvalidLine(number);
            var value = line[(equals + 1)..].Trim();
            if (value.StartsWith('"') || value.StartsWith('\''))
            {
                var quote = value[0];
                var end = value.IndexOf(quote, 1);
                if (end < 0) throw InvalidLine(number);
                var suffix = value[(end + 1)..].TrimStart();
                if (suffix.Length > 0 && !suffix.StartsWith('#')) throw InvalidLine(number);
                value = value[1..end];
            }
            else
            {
                // A hash inside an unquoted value is literal unless preceded by whitespace.
                for (var i = 0; i < value.Length; i++)
                    if (value[i] == '#' && (i == 0 || char.IsWhiteSpace(value[i - 1])))
                    {
                        value = value[..i].TrimEnd();
                        break;
                    }
            }
            _values[name] = value;
        }
    }

    internal string? Get(string name)
    {
        var value = _environment(name) ?? _values.GetValueOrDefault(name);
        if (string.IsNullOrWhiteSpace(value)) return null;
        value = value.Trim();
        if (value.Any(char.IsWhiteSpace) || value.Any(char.IsControl))
            throw new ConfigurationException($"{name} must not contain whitespace or control characters.");
        return value;
    }

    private static ConfigurationException InvalidLine(int line) =>
        new($".env has invalid syntax on line {line}. Use NAME=value or NAME=\"value\".");
}
