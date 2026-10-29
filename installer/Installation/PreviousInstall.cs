// Finds the Filee Rust that is installed already: one installed by this Setup, or by the Inno Setup installer it
// replaced (same uninstall key, "Inno Setup: …" values, unins000.exe). An update keeps the install folder and the
// user's choices, which come from what is on the PC now (the Run value, the 7-Zip folder, the engine-update.disabled
// file) rather than from what was ticked once.
//
// The original Filee has an uninstall key of its own. It is only ever read here (OriginalFileeFolder).

using Filee.Setup.Platform;

namespace Filee.Setup.Installation;

/// <param name="Folder">The install folder (has filee-rust.exe).</param>
/// <param name="Version">DisplayVersion of the uninstall entry.</param>
/// <param name="ByInno">Installed by the Inno Setup installer.</param>
/// <param name="Language">The language of the last install.</param>
/// <param name="Record">uninstall.json of an install by this Setup.</param>
public sealed record PreviousInstall(
    string Folder,
    string? Version,
    InstallScope Scope,
    bool ByInno,
    string? Language,
    InstallRecord? Record)
{
    public static Hive HiveOf(InstallScope scope) => scope == InstallScope.Machine ? Hive.LocalMachine : Hive.CurrentUser;

    /// <summary>The installed Filee Rust for <paramref name="scope"/>, or null (also when its entry points to nothing).</summary>
    public static PreviousInstall? Find(ISetupRegistry registry, InstallScope scope)
    {
        var hive = HiveOf(scope);
        const string key = SetupNames.UninstallKey;
        if (!registry.KeyExists(hive, key))
            return null;
        var folder = registry.Get(hive, key, "InstallLocation")?.Text;
        if (string.IsNullOrWhiteSpace(folder))
            folder = registry.Get(hive, key, SetupNames.InnoAppPathValue)?.Text;
        if (string.IsNullOrWhiteSpace(folder))
            return null;
        folder = Path.TrimEndingDirectorySeparator(Environment.ExpandEnvironmentVariables(folder.Trim().Trim('"')));
        if (!File.Exists(Path.Combine(folder, SetupNames.AppExe)))
            return null;
        var byInno = registry.Get(hive, key, SetupNames.InnoVersionValue) is not null
                     || File.Exists(Path.Combine(folder, SetupNames.InnoUninstallerFiles[0]));
        var language = registry.Get(hive, key, SetupNames.LanguageValue)?.Text ?? registry.Get(hive, key, SetupNames.InnoLanguageValue)?.Text;
        return new PreviousInstall(
            folder,
            registry.Get(hive, key, "DisplayVersion")?.Text,
            scope,
            byInno,
            language,
            InstallRecord.Read(folder));
    }

    /// <summary>
    /// The installed Filee Rust: the scope the command line named, else this user's, else all users' (an Inno install
    /// for everyone is upgraded in place, keeping one entry in Apps &amp; Features).
    /// </summary>
    public static PreviousInstall? FindAny(ISetupRegistry registry, SetupOptions options) =>
        options.ScopeGiven
            ? Find(registry, options.Scope)
            : Find(registry, InstallScope.User) ?? Find(registry, InstallScope.Machine);

    /// <summary>
    /// The files of this installation that an update replaces or removes: the recorded list, or for an Inno Setup
    /// install (which copied the whole stage recursively) everything in the folder, Inno's uninstaller files
    /// included. Paths are relative to <see cref="Folder"/>.
    /// </summary>
    public IReadOnlyList<string> OwnedFiles()
    {
        if (Record is not null)
            return Record.Files;
        if (!ByInno)
            return [];
        return Directory.EnumerateFiles(Folder, "*", SearchOption.AllDirectories)
            .Select(f => Path.GetRelativePath(Folder, f))
            .Where(f => !f.StartsWith(FileSwap.WorkFolderPrefix, StringComparison.OrdinalIgnoreCase))
            .ToList();
    }

    /// <summary>
    /// The options as they are now: the Run value exists or not, the 7-Zip folder is there or not, the weekly engine
    /// update is switched off or not. Engines are not downloaded again by an update.
    /// </summary>
    public SetupTasks CurrentTasks(ISetupRegistry registry) => new(
        registry.Get(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue) is not null,
        Directory.Exists(Path.Combine(Folder, SetupNames.SevenZipFolder)),
        !File.Exists(Path.Combine(Folder, SetupNames.EngineUpdateOffFile)),
        []);

    /// <summary>The original Filee's install folder (HKLM64, else HKCU), or null when it isn't installed. Read only.</summary>
    public static string? OriginalFileeFolder(ISetupRegistry registry)
    {
        foreach (var hive in new[] { Hive.LocalMachine, Hive.CurrentUser })
        {
            var folder = registry.Get(hive, SetupNames.OriginalFileeKey, "InstallLocation")?.Text;
            if (!string.IsNullOrWhiteSpace(folder))
                return Path.TrimEndingDirectorySeparator(folder.Trim().Trim('"'));
        }
        return null;
    }
}
