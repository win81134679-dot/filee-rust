using Filee.Setup;
using Filee.Setup.Installation;
using Filee.Setup.Payload;
using Filee.Setup.Platform;
using Xunit;

namespace FileeRust.Setup.Tests;

/// <summary>Records what Setup asks Windows to do; nothing is run or started.</summary>
internal sealed class FakeSystem : ISetupSystem
{
    public List<(string Exe, string Arguments)> Ran { get; } = [];
    public List<(string Path, string Target, string Arguments, string? Icon)> Shortcuts { get; } = [];

    public void CreateShortcut(string path, string target, string workingFolder, string description, string arguments = "", string? icon = null)
    {
        Shortcuts.Add((path, target, arguments, icon));
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, "lnk");
    }

    public int RunAndWait(string executable, string arguments, TimeSpan timeout)
    {
        Ran.Add((executable, arguments));
        return 0;
    }

    public IReadOnlyList<RunningProgram> ProgramsIn(string folder) => [];

    public void Kill(int processId)
    {
    }

    public void DeleteAtRestart(string path)
    {
    }
}

/// <summary>A scratch machine: sandbox folders, an in-memory registry, a payload built from fake product files.</summary>
internal sealed class Machine : IDisposable
{
    public string Root { get; } = Path.Combine(Path.GetTempPath(), "FileeRustSetupTests-" + Guid.NewGuid().ToString("N")[..8]);
    public SetupPaths Paths { get; }
    public MemoryRegistry Registry { get; } = new();
    public FakeSystem System { get; } = new();
    public SetupLog Log { get; } = new(null);
    public SetupContext Context { get; }
    public string Folder => Path.Combine(Paths.UserPrograms, "Filee Rust");

    public Machine()
    {
        Directory.CreateDirectory(Root);
        Paths = SetupPaths.Sandbox(Root);
        Context = new SetupContext(Registry, System, Paths, Log);
    }

    /// <summary>A Setup carrying a small product: the shell, the hosted app, 7-Zip, the files Inno Setup installed.</summary>
    public SetupPayload Payload(string version = "2.2.0-rust.7")
    {
        var stage = Path.Combine(Root, "stage");
        foreach (var (file, content) in new[]
        {
            ("filee-rust.exe", "shell"), (@"app\Filee.exe", "hosted"), (@"app\filee-cli.exe", "cli"),
            (@"app\engines\7zip\7z.exe", "7z"), (@"app\engines\7zip\7z.dll", "7zdll"),
            (@"i18n\en.json", "{}"), (@"licenses\LICENSE.txt", "mit"), ("README.txt", "readme"),
            ("filee.ico", "ico"), ("version.txt", version + "\n"), ("update.pub", "key"),
        })
        {
            var path = Path.Combine(stage, file);
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, content);
        }
        var program = typeof(SetupTexts).Assembly.Location; // any program file stands in for Setup.exe
        var output = Path.Combine(Root, $"Setup-{version}.exe");
        SetupPayload.Write(program, stage, output, version, [], "", fast: true);
        return SetupPayload.Open(output)!;
    }

    public InstallResult Install(SetupTasks? tasks = null, PreviousInstall? previous = null, InstallScope scope = InstallScope.User,
        string? version = null, string? folder = null)
    {
        using var payload = version is null ? Payload() : Payload(version);
        var choices = new InstallChoices(folder ?? Folder, scope, tasks ?? SetupTasks.Defaults, "en");
        return new Installer(Context, payload).Run(choices, previous);
    }

    public void Dispose()
    {
        try
        {
            Directory.Delete(Root, true);
        }
        catch (IOException)
        {
        }
    }
}

public class InstallTests
{
    private static string Key => SetupNames.UninstallKey;

    [Fact]
    public void FreshInstallDoesEverythingTheInnoOneDid()
    {
        using var m = new Machine();
        var result = m.Install();
        Assert.True(result.Succeeded, result.Error?.ToString());

        // Files: the whole stage, the uninstaller and its list.
        foreach (var f in new[] { "filee-rust.exe", @"app\Filee.exe", @"app\engines\7zip\7z.exe", @"i18n\en.json", "README.txt", "filee.ico",
                     "version.txt", "update.pub", "uninstall.exe", "uninstall.json" })
            Assert.True(File.Exists(Path.Combine(m.Folder, f)), f);

        // Identity: the Inno AppId, so Apps & Features keeps one entry.
        Assert.EndsWith(@"{6B0F3C5E-2D1A-4E57-9E2B-F11EE0717E01}_is1", Key);
        string? Value(string name) => m.Registry.Get(Hive.CurrentUser, Key, name)?.Text;
        Assert.Equal("Filee Rust", Value("DisplayName"));
        Assert.Equal("2.2.0-rust.7", Value("DisplayVersion"));
        Assert.Equal("Filee Rust (based on Filee by Filee contributors, MIT)", Value("Publisher"));
        Assert.Equal("https://github.com/KnifeLemon/Filee", Value("URLInfoAbout"));
        Assert.Equal(Path.Combine(m.Folder, "filee.ico"), Value("DisplayIcon"));
        Assert.Equal(m.Folder + Path.DirectorySeparatorChar, Value("InstallLocation"));
        Assert.Equal("\"" + Path.Combine(m.Folder, "uninstall.exe") + "\"", Value("UninstallString"));
        Assert.False(m.Registry.KeyExists(Hive.LocalMachine, Key));

        // Start at sign-in: filee-rust.exe --background, not upstream's --start-with-windows.
        Assert.Equal("\"" + Path.Combine(m.Folder, "filee-rust.exe") + "\" --background",
            m.Registry.Get(Hive.CurrentUser, SetupNames.RunKey, "FileeRust")?.Text);

        // The weekly update stays on; the shortcut starts the settings window.
        Assert.False(File.Exists(Path.Combine(m.Folder, "engine-update.disabled")));
        var shortcut = Assert.Single(m.System.Shortcuts);
        Assert.Equal(Path.Combine(m.Paths.UserStartMenu, "Filee Rust.lnk"), shortcut.Path);
        Assert.Equal(Path.Combine(m.Folder, "filee-rust.exe"), shortcut.Target);
        Assert.Equal("--settings", shortcut.Arguments);
        Assert.Equal(Path.Combine(m.Folder, "filee.ico"), shortcut.Icon);

        // What an interactive install starts: the six downloads, then the settings window. No README, no choice.
        Assert.Equal("--settings", result.Launch!.Arguments);
        Assert.Equal(Path.Combine(m.Folder, "filee-rust.exe"), result.Launch.Executable);
        Assert.Equal(Path.Combine(m.Folder, "app", "Filee.exe"), result.EngineDownload!.Executable);
        Assert.Equal("--hosted --background --install-engines=libreoffice,tesseract,pandoc,ghostscript,ffmpeg,calibre", result.EngineDownload.Arguments);
    }

    [Fact]
    public void UntickedOptionsAreNotInstalled()
    {
        using var m = new Machine();
        var result = m.Install(new SetupTasks(Startup: false, SevenZip: false, EngineUpdate: false, Engines: []));
        Assert.True(result.Succeeded, result.Error?.ToString());
        Assert.False(Directory.Exists(Path.Combine(m.Folder, "app", "engines", "7zip")));
        Assert.True(File.Exists(Path.Combine(m.Folder, "app", "Filee.exe")));
        Assert.True(File.Exists(Path.Combine(m.Folder, "engine-update.disabled")));
        Assert.Null(m.Registry.Get(Hive.CurrentUser, SetupNames.RunKey, "FileeRust"));
        Assert.Null(result.EngineDownload);
        // The uninstall list does not name what was not installed.
        Assert.DoesNotContain(InstallRecord.Read(m.Folder)!.Files, f => f.Contains("7zip"));
    }

    [Fact]
    public void ReinstallTurnsOptionsBackOn()
    {
        using var m = new Machine();
        m.Install(new SetupTasks(false, false, false, []));
        var previous = PreviousInstall.Find(m.Registry, InstallScope.User);
        Assert.NotNull(previous);
        // The options as they are on the PC now: all off.
        Assert.Equal(new SetupTasks(false, false, false, []).ToInnoString(), previous.CurrentTasks(m.Registry).ToInnoString());
        var result = m.Install(new SetupTasks(true, true, true, ["ffmpeg"]), previous);
        Assert.True(result.Succeeded, result.Error?.ToString());
        Assert.True(File.Exists(Path.Combine(m.Folder, "app", "engines", "7zip", "7z.exe")));
        Assert.False(File.Exists(Path.Combine(m.Folder, "engine-update.disabled")));
        Assert.NotNull(m.Registry.Get(Hive.CurrentUser, SetupNames.RunKey, "FileeRust"));
        Assert.Equal("--hosted --background --install-engines=ffmpeg", result.EngineDownload!.Arguments);
    }

    [Fact]
    public void UpgradeOverAnInnoInstallKeepsOneEntryAndCleansUp()
    {
        using var m = new Machine();
        // What the Inno installer (rust.6) left: Filee Lite's exe and shortcut and Run value, unins000, 7-Zip with an extra
        // file the weekly update wrote, and the Inno values in the uninstall key.
        Directory.CreateDirectory(Path.Combine(m.Folder, "app", "engines", "7zip"));
        foreach (var f in new[] { "filee-lite.exe", "filee-rust.exe", "unins000.exe", "unins000.dat", "old-file.txt", @"app\engines\7zip\7z.exe", @"app\engines\7zip\.installed" })
        {
            Directory.CreateDirectory(Path.GetDirectoryName(Path.Combine(m.Folder, f))!);
            File.WriteAllText(Path.Combine(m.Folder, f), "old");
        }
        File.WriteAllText(Path.Combine(m.Folder, "engine-update.disabled"), "off");
        Directory.CreateDirectory(m.Paths.UserStartMenu);
        File.WriteAllText(Path.Combine(m.Paths.UserStartMenu, "Filee Lite.lnk"), "x");
        m.Registry.SetString(Hive.CurrentUser, SetupNames.RunKey, "FileeLite", "\"old\\filee-lite.exe\" --background");
        m.Registry.SetString(Hive.CurrentUser, Key, "Inno Setup: Setup Version", "6.5.0");
        m.Registry.SetString(Hive.CurrentUser, Key, "InstallLocation", m.Folder + "\\");
        m.Registry.SetString(Hive.CurrentUser, Key, "DisplayVersion", "2.2.0-rust.6");
        m.Registry.SetString(Hive.CurrentUser, Key, "Inno Setup: Selected Tasks", "startup");

        var previous = PreviousInstall.FindAny(m.Registry, SetupOptions.Parse(["/VERYSILENT", "/SELFUPDATE=1"]));
        Assert.NotNull(previous);
        Assert.True(previous.ByInno);
        Assert.Equal(m.Folder, previous.Folder);
        var result = m.Install(new SetupTasks(false, false, true, []), previous);
        Assert.True(result.Succeeded, result.Error?.ToString());

        foreach (var gone in new[] { "filee-lite.exe", "unins000.exe", "unins000.dat", "old-file.txt", @"app\engines\7zip\7z.exe", @"app\engines\7zip\.installed", "engine-update.disabled" })
            Assert.False(File.Exists(Path.Combine(m.Folder, gone)), gone);
        Assert.False(Directory.Exists(Path.Combine(m.Folder, "app", "engines", "7zip")));
        Assert.False(File.Exists(Path.Combine(m.Paths.UserStartMenu, "Filee Lite.lnk")));
        Assert.Null(m.Registry.Get(Hive.CurrentUser, SetupNames.RunKey, "FileeLite"));
        Assert.Equal("shell", File.ReadAllText(Path.Combine(m.Folder, "filee-rust.exe")));

        // One key, rewritten: no Inno values left, the new version in it.
        Assert.Equal("2.2.0-rust.7", m.Registry.Get(Hive.CurrentUser, Key, "DisplayVersion")?.Text);
        Assert.Null(m.Registry.Get(Hive.CurrentUser, Key, "Inno Setup: Setup Version"));
        // The old programs were asked to quit before any file moved.
        Assert.Contains(m.System.Ran, r => r.Exe == Path.Combine(m.Folder, "filee-rust.exe") && r.Arguments == "--quit");
        Assert.Contains(m.System.Ran, r => r.Exe == Path.Combine(m.Folder, "filee-lite.exe") && r.Arguments == "--quit");
    }

    [Fact]
    public void ThePreviousUpdateOwnsOnlyWhatItRecorded()
    {
        using var m = new Machine();
        Assert.True(m.Install().Succeeded);
        File.WriteAllText(Path.Combine(m.Folder, "user-note.txt"), "mine");
        var previous = PreviousInstall.Find(m.Registry, InstallScope.User)!;
        Assert.False(previous.ByInno);
        Assert.True(m.Install(previous: previous, version: "2.2.0-rust.8").Succeeded);
        Assert.True(File.Exists(Path.Combine(m.Folder, "user-note.txt")));
        Assert.Equal("2.2.0-rust.8", File.ReadAllText(Path.Combine(m.Folder, "version.txt")).Trim());
    }

    [Fact]
    public void TheOriginalFileeIsAskedToQuitAndItsKeyIsNeverTouched()
    {
        using var m = new Machine();
        var original = Path.Combine(m.Root, "OriginalFilee");
        Directory.CreateDirectory(original);
        File.WriteAllText(Path.Combine(original, "Filee.exe"), "orig");
        m.Registry.SetString(Hive.LocalMachine, SetupNames.OriginalFileeKey, "InstallLocation", original + "\\");
        m.Registry.SetString(Hive.LocalMachine, SetupNames.OriginalFileeKey, "DisplayName", "Filee");

        Assert.Equal(original, PreviousInstall.OriginalFileeFolder(m.Registry));
        Assert.True(m.Install().Succeeded);
        Assert.Contains(m.System.Ran, r => r.Exe == Path.Combine(original, "Filee.exe") && r.Arguments == "--quit");
        Assert.Equal("Filee", m.Registry.Get(Hive.LocalMachine, SetupNames.OriginalFileeKey, "DisplayName")?.Text);
        Assert.False(m.Registry.KeyExists(Hive.CurrentUser, SetupNames.OriginalFileeKey));

        var previous = PreviousInstall.Find(m.Registry, InstallScope.User)!;
        new Uninstaller(m.Context).Run(m.Folder, InstallRecord.Read(m.Folder)!, Path.Combine(m.Root, "trash"));
        Assert.Equal("Filee", m.Registry.Get(Hive.LocalMachine, SetupNames.OriginalFileeKey, "DisplayName")?.Text);
        Assert.NotNull(previous);
    }

    [Fact]
    public void AnInstalledFileeRustIsFoundInEitherHiveAndTheCommandLineDecides()
    {
        using var m = new Machine();
        Directory.CreateDirectory(m.Folder);
        File.WriteAllText(Path.Combine(m.Folder, "filee-rust.exe"), "x");
        m.Registry.SetString(Hive.LocalMachine, Key, "InstallLocation", m.Folder);
        Assert.Equal(InstallScope.Machine, PreviousInstall.FindAny(m.Registry, SetupOptions.Parse([]))!.Scope);
        Assert.Null(PreviousInstall.FindAny(m.Registry, SetupOptions.Parse(["/CURRENTUSER"])));
        Assert.Equal(InstallScope.Machine, PreviousInstall.FindAny(m.Registry, SetupOptions.Parse(["/ALLUSERS"]))!.Scope);
    }

    [Fact]
    public void FailedInstallPutsEverythingBack()
    {
        using var m = new Machine();
        Assert.True(m.Install().Succeeded);
        var before = File.ReadAllText(Path.Combine(m.Folder, "version.txt"));
        var previous = PreviousInstall.Find(m.Registry, InstallScope.User)!;
        // A file where the 7-Zip folder has to go makes the swap fail halfway.
        File.Delete(Path.Combine(m.Folder, "app", "engines", "7zip", "7z.dll"));
        File.Delete(Path.Combine(m.Folder, "app", "engines", "7zip", "7z.exe"));
        Directory.Delete(Path.Combine(m.Folder, "app", "engines", "7zip"));
        File.WriteAllText(Path.Combine(m.Folder, "app", "engines", "7zip"), "in the way");
        var result = m.Install(previous: previous, version: "2.2.0-rust.9");
        Assert.False(result.Succeeded);
        Assert.True(result.RolledBack);
        Assert.Equal(before, File.ReadAllText(Path.Combine(m.Folder, "version.txt")));
        Assert.Equal("2.2.0-rust.7", m.Registry.Get(Hive.CurrentUser, Key, "DisplayVersion")?.Text);
    }

    [Fact]
    public void UninstallRemovesOursAndLeavesUserData()
    {
        using var m = new Machine();
        Assert.True(m.Install(new SetupTasks(true, true, false, [])).Succeeded);
        // The shell wrote its own verb; the weekly update changed 7-Zip; the user has data.
        m.Registry.SetString(Hive.CurrentUser, SetupNames.ExplorerVerbKey, "MUIVerb", "Filee");
        File.WriteAllText(Path.Combine(m.Folder, "app", "engines", "7zip", "extra.bin"), "x");
        var data = Path.Combine(m.Paths.AppData, "Filee", "settings.json");
        var state = Path.Combine(m.Paths.LocalAppData, "FileeLite", "lite.json");
        foreach (var f in new[] { data, state })
        {
            Directory.CreateDirectory(Path.GetDirectoryName(f)!);
            File.WriteAllText(f, "keep");
        }
        m.System.Ran.Clear();

        var record = InstallRecord.Read(m.Folder)!;
        new Uninstaller(m.Context).Run(m.Folder, record, Path.Combine(m.Root, "trash"));

        Assert.Equal((Path.Combine(m.Folder, "filee-rust.exe"), "--quit"), m.System.Ran[0]);
        Assert.False(Directory.Exists(m.Folder));
        Assert.False(m.Registry.KeyExists(Hive.CurrentUser, Key));
        Assert.Null(m.Registry.Get(Hive.CurrentUser, SetupNames.RunKey, "FileeRust"));
        Assert.False(m.Registry.KeyExists(Hive.CurrentUser, SetupNames.ExplorerVerbKey));
        Assert.False(File.Exists(Path.Combine(m.Paths.UserStartMenu, "Filee Rust.lnk")));
        Assert.True(File.Exists(data));
        Assert.True(File.Exists(state));
    }

    [Fact]
    public void HelpersListWhatToInstallAndRemove()
    {
        Assert.Equal(["a.txt", @"app\x.exe"], Installer.FilesToInstall(["a.txt", @"app\engines\7zip\7z.exe", @"app\x.exe", @"app\engines\7ZIP\7z.dll"], false));
        Assert.Equal(3, Installer.FilesToInstall(["a.txt", @"app\engines\7zip\7z.exe", @"app\x.exe"], true).Count());
        var obsolete = Installer.ObsoleteFiles(["a.txt", "gone.txt", "A.TXT"], ["a.txt"], legacyExePresent: true).ToList();
        Assert.Equal(["gone.txt", "unins000.exe", "unins000.dat", "unins000.msg", "filee-lite.exe"], obsolete);
        Assert.Equal("\"C:\\x y\\filee-rust.exe\" --background", Installer.RunCommand(@"C:\x y"));
        Assert.Null(Installer.EngineRequest(@"C:\x", new SetupTasks(true, true, true, [])));
    }
}

public class ProgramTests
{
    [Fact]
    public void TheWizardsAllUsersChoiceBecomesASilentElevatedRunThatLaunchesAfter()
    {
        var choices = new InstallChoices(@"C:\Program Files\Filee Rust", InstallScope.Machine,
            new SetupTasks(true, false, true, ["ffmpeg"]), "zh-TW", NoIcons: true);
        var args = Program.ElevatedArguments(choices, SetupOptions.Parse([]));
        var parsed = SetupOptions.Parse(args.ToArray());
        Assert.Equal(SetupUi.Silent, parsed.Ui);
        Assert.True(parsed.LaunchAfter);
        Assert.Equal(InstallScope.Machine, parsed.Scope);
        Assert.Equal(@"C:\Program Files\Filee Rust", parsed.Directory);
        Assert.Equal("zh-TW", parsed.Language);
        Assert.True(parsed.NoIcons);
        Assert.Empty(parsed.Unknown);
        Assert.Equal(choices.Tasks.ToInnoString(), SetupTasks.FromCommandLine(parsed.Tasks, null).ToInnoString());
        Assert.False(parsed.SelfUpdate);
    }

    [Fact]
    public void LaunchFileOnlyStartsOurProgramsThatExist()
    {
        var dir = Path.Combine(Path.GetTempPath(), "FileeRustLaunch-" + Guid.NewGuid().ToString("N")[..8]);
        Directory.CreateDirectory(dir);
        try
        {
            var shell = Path.Combine(dir, "filee-rust.exe");
            var hosted = Path.Combine(dir, "Filee.exe");
            var other = Path.Combine(dir, "evil.exe");
            foreach (var f in new[] { shell, hosted, other })
                File.WriteAllText(f, "x");
            var requests = Program.ReadLaunchFile([hosted, "--hosted --install-engines=ffmpeg", other, "", shell, "--settings", Path.Combine(dir, "filee-rust.exe.missing")]);
            Assert.Equal([new LaunchRequest(hosted, "--hosted --install-engines=ffmpeg"), new LaunchRequest(shell, "--settings")], requests);
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void OurTextsReplaceUpstreamsAndBothLanguagesHaveTheSameKeys()
    {
        var en = SetupTexts.LoadOwn("en");
        var zh = SetupTexts.LoadOwn("zh-TW");
        Assert.Equal(en.Keys.Order(), zh.Keys.Order());
        var merged = SetupTexts.Overlaid(SetupTexts.Load("en"), en);
        Assert.Equal("Filee Rust Setup", merged["installer.title"]);
        Assert.Contains("Filee Rust", merged["installer.welcome.fresh"]);
        // Every key the card uses exists in the upstream file or ours.
        foreach (var key in new[] { "installer.install", "installer.update", "installer.status.verifying", "installer.error.unexpected", "common.close" })
            Assert.True(merged.ContainsKey(key), key);
        // Texts that name a product must not still say plain "Filee ".
        foreach (var key in new[] { "installer.title", "installer.progress.install", "installer.done.title", "installer.uninstall.title" })
            Assert.Contains("Filee Rust", merged[key]);
        Assert.Equal("Filee Rust 安裝程式", SetupTexts.Overlaid(SetupTexts.Load("zh-TW"), zh)["installer.title"]);
        foreach (var id in SetupTasks.EngineIds)
            Assert.True(en.ContainsKey("installer.engine." + id) && zh.ContainsKey("installer.engine." + id), id);
    }
}

public class SelfUpdateTests
{
    [Fact]
    public void AfterASelfUpdateTheInstalledShellIsTheOneStartedWhateverHappened()
    {
        using var m = new Machine();
        var options = SetupOptions.Parse(["/VERYSILENT", "/SELFUPDATE=1"]);
        // Nothing installed and nothing in the default folder: nothing to start.
        Assert.Null(Program.SelfUpdateTarget(m.Registry, options, m.Paths));

        // An install by the old Inno Setup in a folder of its own: found through its key, whatever state Setup stopped in.
        var folder = Path.Combine(m.Root, "elsewhere", "Filee Rust");
        Directory.CreateDirectory(folder);
        File.WriteAllText(Path.Combine(folder, "filee-rust.exe"), "x");
        m.Registry.SetString(Hive.CurrentUser, SetupNames.UninstallKey, "InstallLocation", folder + "\\");
        Assert.Equal(Path.Combine(folder, "filee-rust.exe"), Program.SelfUpdateTarget(m.Registry, options, m.Paths));
    }

    [Fact]
    public void WithoutAKeyTheDefaultFolderIsUsed()
    {
        using var m = new Machine();
        Directory.CreateDirectory(m.Folder);
        File.WriteAllText(Path.Combine(m.Folder, "filee-rust.exe"), "x");
        Assert.Equal(Path.Combine(m.Folder, "filee-rust.exe"),
            Program.SelfUpdateTarget(m.Registry, SetupOptions.Parse(["/VERYSILENT", "/SELFUPDATE=1"]), m.Paths));
    }
}
