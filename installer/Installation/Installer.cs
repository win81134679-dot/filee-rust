// Installs or updates Filee Rust, in this order (packaging/FileeRust.iss did the same):
//  1. checks free space and the payload's checksum: nothing has changed yet;
//  2. closes what runs: the original Filee ("Filee.exe --quit", when it is installed), filee-rust.exe and filee-lite.exe
//     ("--quit"), then ends what still runs from the folder (the hosted app\Filee.exe, filee-cli, an engine);
//  3. unpacks into a work folder inside the install folder, checking every file, with the uninstaller and its list;
//  4. swaps the files in (FileSwap: replaced and obsolete files go to a backup, Inno Setup's unins000.* too);
//  5. writes the uninstall entry (the Inno Setup key, so Apps & Features keeps one Filee Rust), the shortcut, the
//     Run value of "start at sign-in", the weekly-update switch;
//  6. deletes the backup, or on any failure in 3-5 puts everything back.
// Starting Filee Rust again (settings window, engine downloads, or --background after a self-update) is Program's job:
// it knows whether a window was shown and who is signed in.

using Filee.Setup.Payload;
using Filee.Setup.Platform;

namespace Filee.Setup.Installation;

public sealed class Installer(SetupContext context, SetupPayload payload)
{
    private ISetupRegistry Registry => context.Registry;
    private ISetupSystem System => context.System;
    private SetupPaths Paths => context.Paths;
    private SetupLog Log => context.Log;

    /// <summary>How long Filee Rust may take to save and exit when asked.</summary>
    public static TimeSpan QuitTimeout { get; } = TimeSpan.FromSeconds(40);

    /// <summary>Space needed besides the files themselves (the uninstaller, Windows' own bookkeeping).</summary>
    private const long SpareSpace = 64L << 20;

    public InstallResult Run(InstallChoices choices, PreviousInstall? previous, IProgress<SetupProgress>? progress = null,
        CancellationToken cancellation = default)
    {
        var folder = Path.TrimEndingDirectorySeparator(Path.GetFullPath(choices.Folder));
        var update = previous is not null && string.Equals(previous.Folder, folder, StringComparison.OrdinalIgnoreCase);
        Log.Info($"{(update ? "Update" : "Install")} of Filee Rust {payload.Manifest.Version} " +
                 $"into {folder} for {(choices.Scope == InstallScope.Machine ? "all users" : "this user")}; " +
                 $"previous: {(previous is null ? "none" : $"{previous.Version} in {previous.Folder}{(previous.ByInno ? " (Inno Setup)" : "")}")}; " +
                 $"options: {choices.Tasks.ToInnoString()}; language: {choices.Language}");
        var warnings = new List<SetupWarning>();
        FileSwap? swap = null;
        var undo = new Stack<(string What, Action Action)>();
        var changing = false;
        var createdFolder = false;
        try
        {
            progress?.Report(new SetupProgress(SetupStep.Verifying, 0));
            CheckSpace(folder);
            payload.Verify(Scaled(progress, SetupStep.Verifying, 0, 0.08), cancellation);
            Log.Info("Payload checksum OK");

            progress?.Report(new SetupProgress(SetupStep.Closing, 0.08));
            CloseRunning(folder, choices.ForceClose);
            cancellation.ThrowIfCancellationRequested();

            // 3. Unpack and check.
            createdFolder = !Directory.Exists(folder);
            Directory.CreateDirectory(folder);
            RemoveLeftoverWorkFolders(folder);
            var work = Path.Combine(folder, FileSwap.WorkFolderPrefix + Guid.NewGuid().ToString("N")[..8]);
            swap = new FileSwap(folder, work, System, Log);
            progress?.Report(new SetupProgress(SetupStep.Copying, 0.12));
            var total = Math.Max(1, payload.Manifest.TotalSize);
            payload.ExtractTo(swap.Staging, new SyncProgress<long>(bytes =>
                progress?.Report(new SetupProgress(SetupStep.Copying, 0.12 + 0.68 * bytes / total))), cancellation);
            Log.Info($"Unpacked and checked {payload.Manifest.Files.Count} files ({payload.Manifest.TotalSize / 1_000_000} MB)");
            var newFiles = FilesToInstall(payload.Manifest.Files.Select(f => f.Path), choices.Tasks.SevenZip).ToList();
            if (!choices.Tasks.SevenZip)
                FileSwap.DeleteTree(Path.Combine(swap.Staging, SetupNames.SevenZipFolder), System, Log);
            payload.CopyProgramTo(Path.Combine(swap.Staging, SetupNames.UninstallerExe));
            newFiles.Add(SetupNames.UninstallerExe);
            newFiles.Add(SetupNames.InstallRecordFile);
            File.WriteAllBytes(Path.Combine(swap.Staging, SetupNames.InstallRecordFile),
                new InstallRecord(payload.Manifest.Version, choices.Scope, newFiles).ToJson());
            var missing = RuntimeCheck.FindMissing(swap.Staging, Paths.SystemFolder);
            Log.Info($"Microsoft Visual C++ runtime check: {(missing.Count == 0 ? "nothing missing" : $"{missing.Count} missing")}");
            if (missing.Count > 0)
            {
                Log.Warn("Missing Microsoft Visual C++ runtime: " + string.Join(", ", missing.Select(m => $"{m.Program} needs {m.Dll}")));
                warnings.Add(new RuntimeWarning(missing));
            }
            cancellation.ThrowIfCancellationRequested();

            // 4. Swap. From here on, a failure puts everything back.
            changing = true;
            progress?.Report(new SetupProgress(SetupStep.Copying, 0.82));
            foreach (var file in newFiles)
                swap.Replace(file);
            var obsolete = ObsoleteFiles(previous?.OwnedFiles() ?? [], newFiles, File.Exists(Path.Combine(folder, SetupNames.LegacyExe)))
                .Where(f => File.Exists(Path.Combine(folder, f)))
                .ToList();
            foreach (var file in obsolete)
                swap.Remove(file);
            Log.Info($"Installed {newFiles.Count} files; {obsolete.Count} files of the previous version are no longer needed");

            // 5. Register.
            progress?.Report(new SetupProgress(SetupStep.Finishing, 0.88));
            Register(folder, choices, undo);

            // 6. Done: the old files go.
            progress?.Report(new SetupProgress(SetupStep.Finishing, 0.97));
            swap.Commit();
            if (!choices.Tasks.SevenZip)
                FileSwap.DeleteTree(Path.Combine(folder, SetupNames.SevenZipFolder), System, Log); // the weekly update may have put files there
            progress?.Report(new SetupProgress(SetupStep.Finishing, 1));
            Log.Info("Done");
            var settings = new LaunchRequest(Path.Combine(folder, SetupNames.AppExe), SetupNames.ShortcutArguments);
            return new InstallResult(settings, warnings, EngineDownload: EngineRequest(folder, choices.Tasks));
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            Log.Error($"Install failed: {ex}");
            if (changing)
                progress?.Report(new SetupProgress(SetupStep.RollingBack, 1));
            while (undo.TryPop(out var step))
            {
                try
                {
                    step.Action();
                }
                catch (Exception undoError) when (undoError is not OutOfMemoryException)
                {
                    Log.Error($"Undo {step.What} failed: {undoError.Message}");
                }
            }
            if (swap is not null)
            {
                if (changing)
                    swap.Rollback();
                else
                    swap.Cleanup();
            }
            if (createdFolder)
                DeleteIfEmpty(folder);
            return new InstallResult(null, warnings, Error: ex, RolledBack: changing);
        }
    }

    /// <summary>The payload's files that are installed: all of them, or all but the bundled 7-Zip.</summary>
    public static IEnumerable<string> FilesToInstall(IEnumerable<string> payloadFiles, bool sevenZip) =>
        payloadFiles.Where(f => sevenZip || !IsInSevenZip(f));

    private static bool IsInSevenZip(string relative) =>
        relative.Replace('/', '\\').StartsWith(SetupNames.SevenZipFolder + "\\", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// What an update removes: the files the previous version owned and this one doesn't ship, Inno Setup's
    /// uninstaller files, and Filee Lite's exe (its name before 2026-10-29) wherever it came from.
    /// </summary>
    public static IEnumerable<string> ObsoleteFiles(IEnumerable<string> previouslyOwned, IReadOnlyCollection<string> newFiles, bool legacyExePresent)
    {
        var owned = previouslyOwned.Concat(SetupNames.InnoUninstallerFiles);
        if (legacyExePresent)
            owned = owned.Append(SetupNames.LegacyExe);
        return owned
            .Except(newFiles, StringComparer.OrdinalIgnoreCase)
            .Where(f => !f.StartsWith(FileSwap.WorkFolderPrefix, StringComparison.OrdinalIgnoreCase))
            .Distinct(StringComparer.OrdinalIgnoreCase);
    }

    /// <summary>Filee's own downloader fetches the ticked engines, after the install, unelevated, without waiting.</summary>
    public static LaunchRequest? EngineRequest(string folder, SetupTasks tasks) => tasks.Engines.Count == 0
        ? null
        : new LaunchRequest(Path.Combine(folder, SetupNames.HostedExe), "--hosted --background --install-engines=" + string.Join(',', tasks.Engines));

    /// <summary>The Run value: the quoted exe and --background (not upstream's --start-with-windows, which would register Filee.exe).</summary>
    public static string RunCommand(string folder) =>
        "\"" + Path.Combine(folder, SetupNames.AppExe) + "\" " + SetupNames.RunArguments;

    private static void DeleteIfEmpty(string folder)
    {
        try
        {
            if (Directory.Exists(folder) && !Directory.EnumerateFileSystemEntries(folder).Any())
                Directory.Delete(folder);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }

    private void CheckSpace(string folder)
    {
        try
        {
            var root = Path.GetPathRoot(folder);
            if (string.IsNullOrEmpty(root) || root.StartsWith(@"\\", StringComparison.Ordinal))
                return;
            var drive = new DriveInfo(root);
            var needed = payload.Manifest.TotalSize + payload.Offset + SpareSpace;
            Log.Info($"Space on {root}: {drive.AvailableFreeSpace / 1_000_000} MB free, {needed / 1_000_000} MB needed");
            if (drive.AvailableFreeSpace < needed)
                throw new SetupException("installer.error.space", $"Not enough space on {root}",
                    root, FormatSize(needed), FormatSize(drive.AvailableFreeSpace));
        }
        catch (Exception ex) when (ex is IOException or ArgumentException or UnauthorizedAccessException)
        {
            Log.Warn($"Could not read the free space: {ex.Message}");
        }
    }

    /// <summary>"1.3 GB", "420 MB" (as the app writes sizes).</summary>
    public static string FormatSize(long bytes) => bytes >= 1_000_000_000
        ? string.Create(global::System.Globalization.CultureInfo.InvariantCulture, $"{bytes / 1_000_000_000.0:0.0} GB")
        : string.Create(global::System.Globalization.CultureInfo.InvariantCulture, $"{Math.Max(1, bytes / 1_000_000)} MB");

    /// <summary>
    /// Asks the original Filee (when installed), Filee Rust and Filee Lite to exit ("--quit": they save first), then
    /// ends whatever still runs from the install folder (the hosted app\Filee.exe, filee-cli, an engine), like Inno
    /// Setup's CloseApplications did.
    /// </summary>
    private void CloseRunning(string folder, bool force)
    {
        // Two programs hooking Ctrl-drag make two donuts: the original Filee goes first.
        if (PreviousInstall.OriginalFileeFolder(Registry) is { } original)
        {
            var exe = Path.Combine(original, SetupNames.OriginalFileeExe);
            if (File.Exists(exe))
                Log.Info($"Original Filee --quit: exit code {System.RunAndWait(exe, "--quit", QuitTimeout)}");
        }
        foreach (var name in new[] { SetupNames.AppExe, SetupNames.LegacyExe })
        {
            var exe = Path.Combine(folder, name);
            if (File.Exists(exe))
                Log.Info($"{name} --quit: exit code {System.RunAndWait(exe, "--quit", QuitTimeout)}");
        }
        var running = System.ProgramsIn(folder).ToList();
        if (running.Count == 0)
            return;
        if (!force)
        {
            Log.Warn($"Still running, left alone (/NOCLOSEAPPLICATIONS): {string.Join(", ", running.Select(p => p.Path))}");
            return;
        }
        foreach (var program in running)
        {
            Log.Info($"Ending {program.Path} ({program.Id})");
            System.Kill(program.Id);
        }
    }

    private void RemoveLeftoverWorkFolders(string folder)
    {
        foreach (var leftover in Directory.EnumerateDirectories(folder, FileSwap.WorkFolderPrefix + "*"))
        {
            Log.Info($"Removing {leftover} (left by an interrupted Setup)");
            FileSwap.DeleteTree(leftover, System, Log);
        }
    }

    private void Register(string folder, InstallChoices choices, Stack<(string, Action)> undo)
    {
        var hive = PreviousInstall.HiveOf(choices.Scope);

        // The uninstall entry: the Inno Setup key, rewritten (its "Inno Setup: …" values and unins000.exe are gone).
        var before = Snapshot(hive, SetupNames.UninstallKey);
        undo.Push(("uninstall entry", () => Restore(hive, SetupNames.UninstallKey, before)));
        WriteUninstallEntry(hive, folder, choices);

        // Start menu shortcut "Filee Rust" → filee-rust.exe --settings. Filee Lite's goes.
        var startMenu = Path.Combine(Paths.StartMenu(choices.Scope), SetupNames.ShortcutName);
        var exe = Path.Combine(folder, SetupNames.AppExe);
        if (!choices.NoIcons)
        {
            if (!File.Exists(startMenu))
                undo.Push(("Start menu shortcut", () => File.Delete(startMenu)));
            System.CreateShortcut(startMenu, exe, folder, SetupNames.AppName, SetupNames.ShortcutArguments, Path.Combine(folder, SetupNames.IconFile));
        }
        foreach (var scope in new[] { InstallScope.User, InstallScope.Machine })
            TryDelete(Path.Combine(Paths.StartMenu(scope), SetupNames.LegacyShortcut));

        // Start at sign-in. The shell never writes this itself (antivirus may block an unsigned program registering its
        // own autostart). Filee Lite's value pointed at the removed filee-lite.exe.
        var runBefore = Registry.Get(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue);
        undo.Push(("Run value", () =>
        {
            if (runBefore is { Value: string old })
                Registry.SetString(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue, old);
            else
                Registry.DeleteValue(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue);
        }));
        if (choices.Tasks.Startup)
            Registry.SetString(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue, RunCommand(folder));
        else
            Registry.DeleteValue(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue);
        Registry.DeleteValue(Hive.CurrentUser, SetupNames.RunKey, SetupNames.LegacyRunValue);

        // filee-rust.exe skips the weekly engine update while this file exists (core::engine_update).
        var flag = Path.Combine(folder, SetupNames.EngineUpdateOffFile);
        if (choices.Tasks.EngineUpdate)
            TryDelete(flag);
        else
            File.WriteAllText(flag, "Weekly engine update turned off in Setup." + Environment.NewLine);
    }

    private void TryDelete(string path)
    {
        try
        {
            if (File.Exists(path))
                File.Delete(path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            Log.Warn($"Could not delete {path}: {ex.Message}");
        }
    }

    private void WriteUninstallEntry(Hive hive, string folder, InstallChoices choices)
    {
        const string key = SetupNames.UninstallKey;
        var version = payload.Manifest.Version;
        var uninstaller = Path.Combine(folder, SetupNames.UninstallerExe);
        Registry.DeleteKey(hive, key);
        Registry.SetString(hive, key, "DisplayName", SetupNames.AppName);
        Registry.SetString(hive, key, "DisplayVersion", version);
        Registry.SetString(hive, key, "Publisher", SetupNames.Publisher);
        Registry.SetString(hive, key, "DisplayIcon", Path.Combine(folder, SetupNames.IconFile));
        Registry.SetString(hive, key, "InstallLocation", folder + Path.DirectorySeparatorChar);
        Registry.SetString(hive, key, "UninstallString", SetupOptions.Quote(uninstaller));
        Registry.SetString(hive, key, "QuietUninstallString", SetupOptions.Quote(uninstaller) + " /VERYSILENT /SUPPRESSMSGBOXES /NORESTART");
        Registry.SetString(hive, key, "URLInfoAbout", SetupNames.AppUrl);
        Registry.SetString(hive, key, "InstallDate", DateTime.Now.ToString("yyyyMMdd", global::System.Globalization.CultureInfo.InvariantCulture));
        Registry.SetNumber(hive, key, "NoModify", 1);
        Registry.SetNumber(hive, key, "NoRepair", 1);
        Registry.SetNumber(hive, key, "EstimatedSize", (int)Math.Min(int.MaxValue, (payload.Manifest.TotalSize + payload.Offset) / 1024));
        if (Version.TryParse(version.Split('-', '+')[0], out var parsed))
        {
            Registry.SetNumber(hive, key, "VersionMajor", parsed.Major);
            Registry.SetNumber(hive, key, "VersionMinor", parsed.Minor);
        }
        Registry.SetString(hive, key, SetupNames.TasksValue, choices.Tasks.ToInnoString());
        Registry.SetString(hive, key, SetupNames.LanguageValue, choices.Language);
        Registry.SetString(hive, key, SetupNames.ScopeValue, choices.Scope == InstallScope.Machine ? "machine" : "user");
        Log.Info($"Uninstall entry written ({(hive == Hive.LocalMachine ? "HKLM" : "HKCU")}\\{key})");
    }

    private Dictionary<string, RegistryEntry>? Snapshot(Hive hive, string key)
    {
        if (!Registry.KeyExists(hive, key))
            return null;
        return Registry.ValueNames(hive, key)
            .Select(name => (name, entry: Registry.Get(hive, key, name)))
            .Where(v => v.entry is not null)
            .ToDictionary(v => v.name, v => v.entry!, StringComparer.OrdinalIgnoreCase);
    }

    private void Restore(Hive hive, string key, Dictionary<string, RegistryEntry>? values)
    {
        Registry.DeleteKey(hive, key);
        foreach (var (name, entry) in values ?? [])
        {
            if (entry.Value is int number)
                Registry.SetNumber(hive, key, name, number);
            else
                Registry.SetString(hive, key, name, (string)entry.Value, entry.Expandable);
        }
    }

    private static IProgress<double>? Scaled(IProgress<SetupProgress>? progress, SetupStep step, double from, double to) =>
        progress is null ? null : new SyncProgress<double>(f => progress.Report(new SetupProgress(step, from + (to - from) * f)));
}

/// <summary>Reports on the calling thread (Progress&lt;T&gt; posts to a synchronization context).</summary>
public sealed class SyncProgress<T>(Action<T> report) : IProgress<T>
{
    public void Report(T value) => report(value);
}
