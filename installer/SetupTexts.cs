// Setup's texts: the app's own translation files (src/Filee.App/Assets/i18n, embedded), so Setup speaks every language
// Filee does in the same words, and over them our own (i18n/en.json, i18n/zh-TW.json: "Filee Rust", the options, the
// engines). Our keys win; languages we have no file for use our English file for the keys it has and upstream's text for the
// rest. Setup's own keys start with "installer."; it also uses the app's names of the options
// and engines. Published as resources, so XAML binds them with DynamicResource like the app does.

using System.Globalization;
using System.Text.Json;
using Avalonia.Controls;

namespace Filee.Setup;

public sealed class SetupTexts
{
    private readonly Dictionary<string, string> _english = Overlaid(Load("en"), LoadOwn("en"));
    private Dictionary<string, string> _current;

    public SetupTexts(string language)
    {
        _current = _english;
        SetLanguage(language);
    }

    /// <summary>The keys of <paramref name="own"/> replace those of <paramref name="upstream"/>.</summary>
    internal static Dictionary<string, string> Overlaid(Dictionary<string, string> upstream, Dictionary<string, string> own)
    {
        foreach (var (key, value) in own)
            upstream[key] = value;
        return upstream;
    }

    /// <summary>Filee's languages (the app's list, AppLanguages) whose translation file Setup carries: all of them.</summary>
    public static IReadOnlyList<(string Code, string NativeName)> Languages { get; } =
        Filee.App.Services.AppLanguages.All
            .Where(l => typeof(SetupTexts).Assembly.GetManifestResourceInfo($"i18n.{l.Code}.json") is not null).ToList();

    public string Language { get; private set; } = "en";

    public event EventHandler? LanguageChanged;

    public string this[string key] =>
        _current.TryGetValue(key, out var value) ? value
        : _english.TryGetValue(key, out var fallback) ? fallback
        : key;

    public string Format(string key, params object[] args)
    {
        try
        {
            return string.Format(CultureInfo.InvariantCulture, this[key], args);
        }
        catch (FormatException)
        {
            return this[key];
        }
    }

    public void SetLanguage(string language)
    {
        Language = Resolve(language, NativeMethodsUiLanguage);
        _current = Language == "en" ? _english : Overlaid(Load(Language), LoadOwn(Language, fallback: "en"));
        LanguageChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Publishes every text into <paramref name="resources"/> (English for missing translations).</summary>
    public void Publish(IResourceDictionary resources)
    {
        foreach (var key in _english.Keys.Concat(_current.Keys).Distinct())
            resources[key] = this[key];
    }

    /// <summary>
    /// A language code for <paramref name="setting"/>: one of <see cref="Languages"/> ("zh-CN", Inno Setup's "zhcn",
    /// "ko-KR" …) or, for "auto" and anything unknown, Windows' display language, falling back to English.
    /// </summary>
    public static string Resolve(string? setting, Func<string> uiLanguage)
    {
        if (!string.IsNullOrWhiteSpace(setting) && !setting.Equals("auto", StringComparison.OrdinalIgnoreCase))
        {
            if (Match(setting) is { } code)
                return code;
        }
        return Match(uiLanguage()) ?? "en";
    }

    private static string? Match(string name)
    {
        var normalized = name.Trim().Replace('_', '-');
        if (normalized.Equals("zhcn", StringComparison.OrdinalIgnoreCase))
            normalized = "zh-CN";
        else if (normalized.Equals("zhtw", StringComparison.OrdinalIgnoreCase))
            normalized = "zh-TW"; // Inno Setup's name for it ("Inno Setup: Language" of an Inno install)
        foreach (var (code, _) in Languages)
        {
            if (normalized.Equals(code, StringComparison.OrdinalIgnoreCase))
                return code;
        }
        if (Filee.App.Services.AppLanguages.PortugueseVariant(normalized) is { } portuguese)
            return portuguese;
        // "zh-Hant-HK" / "zh-HK" → zh-TW; the primary-language fallback below gives the other Chinese names zh-CN.
        if (Filee.App.Services.AppLanguages.IsTraditionalChinese(normalized))
            return "zh-TW";
        // "ko-KR" → ko, "zh-Hans-CN" → zh-CN, "es-MX" → es.
        var primary = normalized.Split('-')[0];
        foreach (var (code, _) in Languages)
        {
            if (code.Split('-')[0].Equals(primary, StringComparison.OrdinalIgnoreCase))
                return code;
        }
        return null;
    }

    private static string NativeMethodsUiLanguage() =>
        OperatingSystem.IsWindows() ? Platform.NativeMethods.UiLanguage() : CultureInfo.InstalledUICulture.Name;

    /// <summary>Our own texts for <paramref name="code"/>; without a file of that language, those of <paramref name="fallback"/>.</summary>
    internal static Dictionary<string, string> LoadOwn(string code, string? fallback = null) =>
        Read(typeof(SetupTexts).Assembly.GetManifestResourceStream($"rust.i18n.{code}.json")
             ?? (fallback is null ? null : typeof(SetupTexts).Assembly.GetManifestResourceStream($"rust.i18n.{fallback}.json")));

    internal static Dictionary<string, string> Load(string code) =>
        Read(typeof(SetupTexts).Assembly.GetManifestResourceStream($"i18n.{code}.json"));

    private static Dictionary<string, string> Read(Stream? stream)
    {
        using var _ = stream;
        if (stream is null)
            return [];
        using var json = JsonDocument.Parse(stream);
        return json.RootElement.EnumerateObject()
            .Where(p => p.Value.ValueKind == JsonValueKind.String)
            .ToDictionary(p => p.Name, p => p.Value.GetString() ?? "");
    }
}
