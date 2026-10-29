// Removes Filee Rust:
//  1. "filee-rust.exe --quit" (and filee-lite.exe's, if one is left): the shell saves and exits;
//  2. ends what still runs from the install folder (the hosted app\Filee.exe, filee-cli, an engine);
//  3. removes the shortcut, the "start at sign-in" Run value, the Explorer verb the shell wrote
//     (HKCU\Software\Classes\*\shell\FileeLite), the bundled 7-Zip folder (the weekly update may have changed it),
//     engine-update.disabled, the files of uninstall.json, the install folder and the uninstall entry;
//  4. leaves the user's data alone: %APPDATA%\Filee (settings, presets) and %LOCALAPPDATA%\FileeLite (the shell's state)
//     and the engines Filee downloaded stay, as with the Inno Setup uninstaller.
// A file Windows keeps in use (the running uninstall.exe) is moved out of the folder into the trash folder and
// deleted at the next restart, so the folder itself goes now.

using Filee.Setup.Platform;

namespace Filee.Setup.Installation;

public sealed class Uninstaller(SetupContext context)
{
    private ISetupRegistry Registry => context.Registry;
    private ISetupSystem System => context.System;
    private SetupLog Log => context.Log;

    /// <param name="trash">A folder outside the install folder (same volume if possible) for files still in use.</param>
    /// <param name="releaseUninstaller">
    /// Called when only uninstall.exe and the folder are left: the uninstaller that started this one from the install
    /// folder is told it worked and ends, so its uninstall.exe can be deleted (a single-file program loads parts of
    /// itself from its file while it runs: that file must not move away under it).
    /// </param>
    /// <returns>Files and folders that are deleted at the next restart.</returns>
    public IReadOnlyList<string> Run(string folder, InstallRecord record, string trash,
        IProgress<SetupProgress>? progress = null, Action? releaseUninstaller = null)
    {
        folder = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder));
        Log.Info($"Uninstalling Filee Rust {record.Version} from {folder} ({record.Scope})");
        var later = new List<string>();
        progress?.Report(new SetupProgress(SetupStep.Closing, 0));

        foreach (var name in new[] { SetupNames.AppExe, SetupNames.LegacyExe })
        {
            var exe = Path.Combine(folder, name);
            if (File.Exists(exe))
                Log.Info($"{name} --quit: exit code {System.RunAndWait(exe, "--quit", Installer.QuitTimeout)}");
        }
        foreach (var program in System.ProgramsIn(folder).Where(p => p.Id != Environment.ProcessId))
        {
            // Not the uninstaller that started this one: it waits for this one and ends by itself.
            if (Path.GetFileName(program.Path).Equals(SetupNames.UninstallerExe, StringComparison.OrdinalIgnoreCase))
                continue;
            Log.Info($"Ending {program.Path} ({program.Id})");
            System.Kill(program.Id);
        }
        progress?.Report(new SetupProgress(SetupStep.Removing, 0.3));

        var hive = PreviousInstall.HiveOf(record.Scope);
        DeleteFile(Path.Combine(context.Paths.StartMenu(record.Scope), SetupNames.ShortcutName), trash, later);
        DeleteFile(Path.Combine(context.Paths.StartMenu(record.Scope), SetupNames.LegacyShortcut), trash, later);
        Registry.DeleteValue(Hive.CurrentUser, SetupNames.RunKey, SetupNames.RunValue);
        Registry.DeleteValue(Hive.CurrentUser, SetupNames.RunKey, SetupNames.LegacyRunValue);
        Registry.DeleteKey(Hive.CurrentUser, SetupNames.ExplorerVerbKey);
        Log.Info("Removed the Run value and the Explorer verb");

        progress?.Report(new SetupProgress(SetupStep.Removing, 0.5));
        FileSwap.DeleteTree(Path.Combine(folder, SetupNames.SevenZipFolder), System, Log);
        var files = record.Files.Concat([SetupNames.InstallRecordFile, SetupNames.EngineUpdateOffFile, SetupNames.LegacyExe])
            .Concat(SetupNames.InnoUninstallerFiles)
            .Where(f => !f.Equals(SetupNames.UninstallerExe, StringComparison.OrdinalIgnoreCase))
            .Distinct(StringComparer.OrdinalIgnoreCase).ToList();
        for (var i = 0; i < files.Count; i++)
        {
            DeleteFile(Path.Combine(folder, files[i]), trash, later);
            progress?.Report(new SetupProgress(SetupStep.Removing, 0.5 + 0.4 * (i + 1) / files.Count));
        }
        // Left by an interrupted Setup.
        if (Directory.Exists(folder))
        {
            foreach (var work in Directory.EnumerateDirectories(folder, FileSwap.WorkFolderPrefix + "*"))
                FileSwap.DeleteTree(work, System, Log);
        }

        Registry.DeleteKey(hive, SetupNames.UninstallKey);
        progress?.Report(new SetupProgress(SetupStep.Removing, 0.95));
        releaseUninstaller?.Invoke();
        DeleteFile(Path.Combine(folder, SetupNames.UninstallerExe), trash, later);
        FileSwap.RemoveEmptyFolders(folder);
        try
        {
            if (Directory.Exists(folder))
                Directory.Delete(folder);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Something that isn't Filee's is still in it (a folder the user chose), or a file is in use.
            Log.Warn($"{folder} stays: {ex.Message}");
            if (Directory.Exists(folder) && !Directory.EnumerateFileSystemEntries(folder).Any())
            {
                System.DeleteAtRestart(folder);
                later.Add(folder);
            }
        }
        progress?.Report(new SetupProgress(SetupStep.Removing, 1));
        Log.Info("Uninstalled");
        return later;
    }

    /// <summary>Deletes a file; one in use is moved into <paramref name="trash"/> (or renamed in place) and deleted at the next restart.</summary>
    private void DeleteFile(string path, string trash, List<string> later)
    {
        if (!File.Exists(path))
            return;
        try
        {
            File.SetAttributes(path, FileAttributes.Normal);
            File.Delete(path);
            return;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            Log.Info($"In use: {path} ({ex.Message})");
        }
        var aside = Path.Combine(trash, $"{Guid.NewGuid():N}-{Path.GetFileName(path)}");
        try
        {
            Directory.CreateDirectory(trash);
            File.Move(path, aside);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Another volume: renaming works only on the same one. Stay in the folder under another name.
            aside = path + "." + Guid.NewGuid().ToString("N")[..8] + ".old";
            try
            {
                File.Move(path, aside);
            }
            catch (Exception again) when (again is IOException or UnauthorizedAccessException)
            {
                Log.Warn($"Could not move {path} aside: {again.Message} ({ex.Message})");
                aside = path;
            }
        }
        System.DeleteAtRestart(aside);
        later.Add(aside);
    }
}
