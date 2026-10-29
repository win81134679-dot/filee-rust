// Entry point of Filee Rust Setup (FileeRust-<version>-Setup.exe, which carries a payload) and of the uninstaller
// (uninstall.exe, the same program without one). SetupStart tells them apart, and from a Setup.exe whose download
// didn't finish.
//
// Rights: the program starts without administrator rights (asInvoker) and installs for the current user. An install for
// all users starts itself again with them (one UAC prompt) and waits; the elevated copy writes what to start at the end
// into a file, and this unelevated one starts it as the signed-in user: Filee Rust must not run elevated (Windows
// blocks drag and drop from Explorer into elevated windows). The uninstaller first copies itself to %TEMP% and runs
// from there (with administrator rights when needed), so the install folder can be deleted.
//
// What starts at the end (Inno Setup's [Run] and CurStepChanged did the same):
//   wizard install        engine downloads (when ticked), then filee-rust.exe --settings, both unelevated
//   /SILENT, /VERYSILENT  nothing, except /SELFUPDATE=1: filee-rust.exe --background, whatever happened

using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using Avalonia;
using Filee.Setup.Installation;
using Filee.Setup.Payload;
using Filee.Setup.Platform;
using Filee.Setup.ViewModels;

namespace Filee.Setup;

[System.Runtime.Versioning.SupportedOSPlatform("windows")]
internal static class Program
{
    private const int ExitOk = 0;
    private const int ExitInitFailed = 1;
    private const int ExitCancelled = 2;
    private const int ExitFailed = 4;

    /// <summary>Win32 ERROR_CANCELLED: the user declined the UAC prompt.</summary>
    private const int ErrorCancelled = 1223;

    [STAThread]
    public static int Main(string[] args)
    {
        if (!OperatingSystem.IsWindows())
            return ExitInitFailed;
        // Never hold a folder Setup may delete (the install folder) as the current directory.
        Environment.CurrentDirectory = Environment.SystemDirectory;
        NativeMethods.QuietErrors();
        var options = SetupOptions.Parse(args);
        var exe = Environment.ProcessPath ?? Path.Combine(AppContext.BaseDirectory, "uninstall.exe");
        var sandbox = options.Sandbox is { } root ? Path.GetFullPath(root) : null;
        var paths = sandbox is null ? SetupPaths.Real() : SetupPaths.Sandbox(sandbox);
        Directory.CreateDirectory(paths.Temp);

        var start = SetupStart.Of(exe, options.Uninstall);
        var payload = start.Payload;
        var uninstall = start.Mode == StartMode.Uninstall;
        var log = new SetupLog(options.LogFile ?? SetupLog.DefaultFile(paths.Temp, uninstall ? "Filee-Uninstall" : "Filee-Setup"));
        log.Info($"{(uninstall ? "Uninstaller" : "Setup")} {SetupInfo.Version}{(payload is null ? "" : $" with Filee Rust {payload.Manifest.Version}")}: " +
                 $"{exe} {SetupOptions.Join(args)}; administrator: {NativeMethods.IsElevated()}; Windows {Environment.OSVersion.Version} " +
                 $"{RuntimeInformation.OSArchitecture}{(sandbox is null ? "" : $"; sandbox {sandbox}")}");
        foreach (var unknown in options.Unknown)
            log.Warn($"Unknown argument ignored: {unknown}");
        if (options.SelfUpdate)
            log.Info("Self-update: Filee Rust is started again at the very end, whatever happens");
        foreach (var ignored in options.Ignored)
            log.Info($"Accepted, changes nothing: {ignored}");
        foreach (var task in SetupTasks.UnknownNames(options.Tasks).Concat(SetupTasks.UnknownNames(options.MergeTasks)))
            log.Warn($"Unknown task ignored: {task}");

        var texts = new SetupTexts(options.Language ?? "auto");
        try
        {
            if (!IsSupportedWindows())
            {
                log.Error("Windows is too old or not 64-bit");
                Tell(options, texts["installer.error.windows"], NativeMethods.MB_ICONERROR);
                return ExitInitFailed;
            }
            if (start.Mode == StartMode.Damaged)
            {
                log.Error($"Damaged payload: {start.Problem}");
                Tell(options, texts["installer.error.damaged"], NativeMethods.MB_ICONERROR);
                return ExitInitFailed;
            }
            if (start.Mode == StartMode.Incomplete)
            {
                log.Error($"Incomplete download: {start.Problem}");
                Tell(options, texts["installer.error.incomplete"], NativeMethods.MB_ICONERROR);
                return ExitInitFailed;
            }
            return uninstall
                ? Uninstall(args, options, exe, paths, sandbox, log, texts)
                : Install(args, options, exe, payload!, paths, sandbox, log, texts);
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            log.Error($"Setup failed: {ex}");
            Tell(options, texts.Format("installer.error.unexpected", ex.Message), NativeMethods.MB_ICONERROR);
            return ExitFailed;
        }
        finally
        {
            payload?.Dispose();
            if (options.SelfUpdate && !uninstall && sandbox is null)
                RestartAfterSelfUpdate(options, paths, log);
        }
    }

    /// <summary>Windows 10 1809 or later, 64-bit (x64, or ARM64 running x64 programs).</summary>
    private static bool IsSupportedWindows() =>
        Environment.OSVersion.Version.Build >= SetupNames.MinimumBuild
        && RuntimeInformation.OSArchitecture is Architecture.X64 or Architecture.Arm64;

    private static int Install(string[] args, SetupOptions options, string exe, SetupPayload payload, SetupPaths paths, string? sandbox,
        SetupLog log, SetupTexts texts)
    {
        var context = CreateContext(sandbox, paths, log);
        var previous = PreviousInstall.FindAny(context.Registry, options);
        var scope = previous?.Scope ?? options.Scope;
        if (sandbox is null && scope == InstallScope.Machine && !NativeMethods.IsElevated())
        {
            payload.Dispose();
            return RunElevated(exe, args, log, launchFile: NewLaunchFile());
        }

        if (options.Language is null && previous?.Language is { } recorded)
            texts.SetLanguage(recorded);
        var folder = previous?.Folder
                     ?? (options.Directory is { } dir ? Path.GetFullPath(Environment.ExpandEnvironmentVariables(dir)) : null)
                     ?? paths.DefaultInstallFolder(scope);
        if (previous is not null && options.Directory is { } asked
            && !Path.GetFullPath(Environment.ExpandEnvironmentVariables(asked)).TrimEnd('\\').Equals(previous.Folder, StringComparison.OrdinalIgnoreCase))
            log.Warn($"/DIR={asked} ignored: Filee Rust is installed in {previous.Folder}, which an update keeps");

        // A fresh install ticks everything; an update shows what is on the PC now and downloads no engines again.
        var tasks = SetupTasks.FromCommandLine(options.Tasks, options.MergeTasks, previous?.CurrentTasks(context.Registry));
        var choices = new InstallChoices(folder, scope, tasks, texts.Language, NoIcons: options.NoIcons, ForceClose: options.ForceCloseApplications);
        var runner = new SetupRunner(context, payload, previous);

        InstallResult? result;
        int exitCode;
        var launches = new List<LaunchRequest>();
        if (options.Ui == SetupUi.VerySilent)
        {
            result = runner.Install(choices, new SyncProgress<SetupProgress>(_ => { }));
            exitCode = result.Succeeded ? ExitOk : ExitFailed;
            if (options.LaunchAfter && result.Succeeded)
                launches.AddRange(Launches(result.EngineDownload, result.Launch));
        }
        else
        {
            // Like Inno Setup's InitializeWizard: say it before the wizard, never in silent mode.
            if (options.Ui == SetupUi.Wizard && PreviousInstall.OriginalFileeFolder(context.Registry) is not null)
                Tell(options, texts["installer.original_filee"], NativeMethods.MB_ICONINFORMATION);
            var model = new SetupViewModel(texts, runner, payload.Manifest.Version, choices, previous, paths, NativeMethods.IsElevated(),
                payload.Manifest.TotalSize + payload.Offset, languagePicked: options.Language is not null)
            {
                LogFile = log.File,
            };
            RunWindow(model, texts, options.Ui == SetupUi.Silent);
            if (model.ElevateWith is { } elevate)
            {
                // "All users" was ticked: the same install again, with administrator rights and no questions.
                payload.Dispose();
                return RunElevated(exe, ElevatedArguments(elevate, options), log, launchFile: NewLaunchFile());
            }
            result = model.Result;
            exitCode = model.ExitCode;
            if (options.Ui == SetupUi.Wizard)
                launches.AddRange(Launches(model.EngineDownload, model.Launch));
            else if (options.LaunchAfter && result is { Succeeded: true })
                launches.AddRange(Launches(result.EngineDownload, result.Launch));
        }

        if (result is { Succeeded: false } failed && options.IsSilent)
        {
            Tell(options, SetupMessages.Error(texts, failed.Error!) + Environment.NewLine + Environment.NewLine + log.File, NativeMethods.MB_ICONERROR);
        }
        if (result is { Succeeded: true } && options.Ui == SetupUi.Silent)
        {
            // /SILENT shows its window anyway: say what doesn't work.
            foreach (var warning in result.Warnings.OfType<RuntimeWarning>())
                Tell(options, SetupMessages.Warning(texts, warning), NativeMethods.MB_ICONWARNING);
        }
        if (result is { Succeeded: true } && launches.Count > 0)
        {
            if (options.NoLaunch || sandbox is not null)
                log.Info($"Not starting Filee Rust ({(sandbox is null ? "/NOLAUNCH" : "sandbox")}): {string.Join("; ", launches.Select(l => l.Executable + " " + l.Arguments))}");
            else
                StartAll(launches, options.LaunchFile, log);
        }
        log.Info($"Exit code {exitCode}");
        return exitCode;
    }

    private static string NewLaunchFile() => Path.Combine(Path.GetTempPath(), $"Filee-Setup-{Guid.NewGuid():N}.launch");

    /// <summary>The engine downloads first, then the settings window (Inno Setup's order).</summary>
    private static IEnumerable<LaunchRequest> Launches(LaunchRequest? engines, LaunchRequest? settings) =>
        new[] { engines, settings }.OfType<LaunchRequest>();

    /// <summary>The command line of the elevated copy that does what the wizard's "all users" choice asked for.</summary>
    internal static IReadOnlyList<string> ElevatedArguments(InstallChoices choices, SetupOptions options)
    {
        var args = new List<string>
        {
            "/SILENT", "/LAUNCHAFTER", "/ALLUSERS", $"/DIR={choices.Folder}", $"/LANG={choices.Language}", $"/TASKS={choices.Tasks.ToInnoString()}",
        };
        if (choices.NoIcons)
            args.Add("/NOICONS");
        if (!choices.ForceClose)
            args.Add("/NOCLOSEAPPLICATIONS");
        if (options.SuppressMessageBoxes)
            args.Add("/SUPPRESSMSGBOXES");
        if (options.NoLaunch)
            args.Add("/NOLAUNCH");
        return args;
    }

    /// <summary>
    /// /SELFUPDATE=1: Filee Rust let go of its lock and quit before it ran this Setup, so whatever happened here (success,
    /// failure, a window that never opened) it is started again with --background. The folder comes from the registry.
    /// </summary>
    private static void RestartAfterSelfUpdate(SetupOptions options, SetupPaths paths, SetupLog log)
    {
        try
        {
            if (SelfUpdateTarget(new WindowsRegistry(), options, paths) is not { } app)
            {
                log.Warn("Self-update: filee-rust.exe is not where Setup would put it, nothing to start");
                return;
            }
            StartAll([new LaunchRequest(app, SetupNames.RunArguments)], launchFile: null, log);
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            log.Warn($"Self-update: could not start Filee Rust again: {ex.Message}");
        }
    }

    /// <summary>
    /// The filee-rust.exe to start after a self-update: the installed one (registry), else the one in the default or /DIR
    /// folder; null when there is none.
    /// </summary>
    internal static string? SelfUpdateTarget(ISetupRegistry registry, SetupOptions options, SetupPaths paths)
    {
        var folder = PreviousInstall.FindAny(registry, options)?.Folder
                     ?? (options.Directory is { } dir ? Path.GetFullPath(Environment.ExpandEnvironmentVariables(dir)) : paths.DefaultInstallFolder(options.Scope));
        var app = Path.Combine(folder, SetupNames.AppExe);
        return File.Exists(app) ? app : null;
    }

    private static int Uninstall(string[] args, SetupOptions options, string exe, SetupPaths paths, string? sandbox, SetupLog log, SetupTexts texts)
    {
        var folder = Path.TrimEndingDirectorySeparator(Path.GetFullPath(options.Directory ?? Path.GetDirectoryName(exe)!));
        var record = InstallRecord.Read(folder);
        if (record is null)
        {
            log.Error($"No {SetupNames.InstallRecordFile} in {folder}");
            Tell(options, texts.Format("installer.uninstall.not_installed", folder), NativeMethods.MB_ICONERROR);
            return ExitInitFailed;
        }
        var context = CreateContext(sandbox, paths, log);
        var hive = PreviousInstall.HiveOf(record.Scope);
        if (options.Language is null && context.Registry.Get(hive, SetupNames.UninstallKey, SetupNames.LanguageValue)?.Text is { } language)
            texts.SetLanguage(language);
        var needsAdmin = record.Scope == InstallScope.Machine && sandbox is null && !NativeMethods.IsElevated();

        // Phase 1: running from the install folder, which can't be deleted while this runs. Copy to %TEMP% and wait.
        var exeFolder = Path.TrimEndingDirectorySeparator(Path.GetDirectoryName(exe)!);
        if (exeFolder.Equals(folder, StringComparison.OrdinalIgnoreCase))
        {
            var temp = Path.Combine(paths.Temp, $"Filee-Uninstall-{Guid.NewGuid():N}");
            Directory.CreateDirectory(temp);
            var copy = Path.Combine(temp, SetupNames.UninstallerExe);
            File.Copy(exe, copy);
            var childArgs = args.Where(a => !HasName(a, "/DIR") && !HasName(a, "/LOG") && !HasName(a, "/UNINSTALL") && !HasName(a, "/FIRSTPHASE"))
                .Concat(["/UNINSTALL", $"/DIR={folder}", $"/LOG={log.File}", $"/FIRSTPHASE={Environment.ProcessId}"]);
            log.Info($"Continuing from {copy}");
            // Waits for the copy, or for its word that only this uninstall.exe is left (it then deletes it).
            var released = Path.Combine(temp, ReleasedFile);
            var code = RunAndWait(copy, childArgs, elevated: needsAdmin, log, until: () => File.Exists(released));
            if (code == ExitCancelled)
                TryDeleteFolder(temp);
            return code;
        }
        if (needsAdmin)
            return RunElevated(exe, args, log, launchFile: null);

        // Phase 2: from a copy in %TEMP%.
        var fromTemp = Path.GetFileName(exeFolder).StartsWith("Filee-Uninstall-", StringComparison.OrdinalIgnoreCase);
        var trash = fromTemp ? Path.Combine(exeFolder, "trash") : Path.Combine(paths.Temp, $"Filee-Uninstall-{Guid.NewGuid():N}");
        var runner = new SetupRunner(context, null, null, folder, record, trash)
        {
            ReleaseFirstPhase = options.FirstPhase is { } first && fromTemp ? () => ReleaseFirstPhase(exeFolder, first, log) : null,
        };
        int exitCode;
        if (options.Ui == SetupUi.VerySilent)
        {
            exitCode = runner.Uninstall(new SyncProgress<SetupProgress>(_ => { })) is { } error
                ? Fail(error)
                : ExitOk;
        }
        else
        {
            var model = new SetupViewModel(texts, runner, record.Version, new InstallChoices(folder, record.Scope, SetupTasks.Defaults, texts.Language),
                null, paths, NativeMethods.IsElevated(), 0, uninstall: true)
            {
                LogFile = log.File,
            };
            RunWindow(model, texts, options.Ui == SetupUi.Silent);
            exitCode = model.ExitCode;
            if (exitCode == 1 && options.IsSilent)
                Tell(options, model.ErrorText, NativeMethods.MB_ICONERROR);
        }
        // This copy and what was in use go once it has ended (and at the next restart, to be sure).
        CleanUpAfterExit(fromTemp ? exeFolder : trash, context.System, log);
        log.Info($"Exit code {exitCode}");
        return exitCode;

        int Fail(Exception error)
        {
            Tell(options, SetupMessages.Error(texts, error), NativeMethods.MB_ICONERROR);
            return ExitInitFailed;
        }
    }

    /// <summary>Written by the uninstaller's copy when only uninstall.exe is left (see <see cref="Uninstaller.Run"/>).</summary>
    private const string ReleasedFile = "released";

    /// <summary>Tells the uninstaller in the install folder that it may end, and waits until it has.</summary>
    private static void ReleaseFirstPhase(string temp, int processId, SetupLog log)
    {
        try
        {
            using var first = Process.GetProcessById(processId);
            File.WriteAllText(Path.Combine(temp, ReleasedFile), "0");
            if (!first.WaitForExit(TimeSpan.FromSeconds(30)))
                log.Warn("The first uninstaller did not end");
        }
        catch (ArgumentException)
        {
            // Ended already.
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or Win32Exception or InvalidOperationException)
        {
            log.Warn($"Could not release the first uninstaller: {ex.Message}");
        }
    }

    private static SetupContext CreateContext(string? sandbox, SetupPaths paths, SetupLog log)
    {
        if (sandbox is null)
            return new SetupContext(new WindowsRegistry(), new WindowsSetupSystem(log), paths, log);
        // The sandbox's registry is a JSON file.
        return new SetupContext(new MemoryRegistry(Path.Combine(sandbox, "registry.json")),
            new SandboxSetupSystem(log, paths.SettingsFolder), paths, log, Sandbox: true);
    }

    private static void RunWindow(SetupViewModel model, SetupTexts texts, bool silent)
    {
        App.Model = model;
        App.Texts = texts;
        App.Silent = silent;
        BuildAvaloniaApp().StartWithClassicDesktopLifetime([]);
    }

    /// <summary>Also used by the XAML previewer and the tests' headless platform (with their own builder).</summary>
    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .LogToTrace();

    /// <summary>
    /// Starts this program again with administrator rights and waits. With <paramref name="launchFile"/>, starts what
    /// the elevated copy wrote into it afterwards, as the signed-in user.
    /// </summary>
    private static int RunElevated(string exe, IEnumerable<string> args, SetupLog log, string? launchFile)
    {
        var childArgs = args.Where(a => !HasName(a, "/LAUNCHFILE") && !HasName(a, "/LOG"))
            .Concat(launchFile is null ? [] : [$"/LAUNCHFILE={launchFile}"])
            .Append($"/LOG={log.File}");
        log.Info("Asking for administrator rights");
        var code = RunAndWait(exe, childArgs, elevated: true, log);
        if (launchFile is not null && File.Exists(launchFile))
        {
            try
            {
                var lines = File.ReadAllLines(launchFile);
                File.Delete(launchFile);
                var requests = ReadLaunchFile(lines);
                if (requests.Count == 0)
                    log.Warn($"Not starting what {launchFile} names");
                else
                    StartAll(requests, launchFile: null, log);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                log.Warn($"Could not read {launchFile}: {ex.Message}");
            }
        }
        return code;
    }

    /// <summary>
    /// The launch file: pairs of lines (program, arguments). Only programs of ours that exist are started: the file is in
    /// a folder other programs can write to.
    /// </summary>
    internal static IReadOnlyList<LaunchRequest> ReadLaunchFile(IReadOnlyList<string> lines)
    {
        var requests = new List<LaunchRequest>();
        for (var i = 0; i < lines.Count; i += 2)
        {
            var program = lines[i];
            var name = Path.GetFileName(program);
            if (!(name.Equals(SetupNames.AppExe, StringComparison.OrdinalIgnoreCase) || name.Equals("Filee.exe", StringComparison.OrdinalIgnoreCase))
                || !File.Exists(program))
                continue;
            requests.Add(new LaunchRequest(program, i + 1 < lines.Count ? lines[i + 1] : ""));
        }
        return requests;
    }

    /// <param name="until">Stops waiting early when true (checked every 100 ms); the exit code is then 0.</param>
    private static int RunAndWait(string exe, IEnumerable<string> args, bool elevated, SetupLog log, Func<bool>? until = null)
    {
        var info = new ProcessStartInfo(exe, SetupOptions.Join(args))
        {
            UseShellExecute = true,
            WorkingDirectory = Environment.SystemDirectory,
        };
        if (elevated)
            info.Verb = "runas";
        try
        {
            using var child = Process.Start(info);
            if (child is null)
                return ExitInitFailed;
            while (!child.WaitForExit(100))
            {
                if (until?.Invoke() == true)
                {
                    log.Info($"{Path.GetFileName(exe)} is done with this one");
                    return ExitOk;
                }
            }
            log.Info($"{Path.GetFileName(exe)} ended with exit code {child.ExitCode}");
            return child.ExitCode;
        }
        catch (Win32Exception ex) when (ex.NativeErrorCode == ErrorCancelled)
        {
            log.Info("Administrator rights declined");
            return ExitCancelled;
        }
        catch (Win32Exception ex)
        {
            log.Error($"Could not start {exe}: {ex.Message}");
            return ExitInitFailed;
        }
    }

    private static bool HasName(string arg, string name) =>
        arg.Equals(name, StringComparison.OrdinalIgnoreCase) || arg.StartsWith(name + "=", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Starts programs as the signed-in user: through the unelevated Setup that started this one (the launch file), else
    /// with Explorer's token when this Setup is elevated, else directly.
    /// </summary>
    private static void StartAll(IReadOnlyList<LaunchRequest> launches, string? launchFile, SetupLog log)
    {
        if (launchFile is not null)
        {
            try
            {
                File.WriteAllLines(launchFile, launches.SelectMany(l => new[] { l.Executable, l.Arguments }));
                log.Info($"Filee Rust starts from the unelevated Setup: {string.Join("; ", launches.Select(l => l.Arguments))}");
                return;
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                log.Warn($"Could not write {launchFile}: {ex.Message}");
            }
        }
        foreach (var launch in launches)
        {
            if (NativeMethods.IsElevated())
            {
                if (NativeMethods.StartAsShellUser(launch.Executable, launch.Arguments, Path.GetDirectoryName(launch.Executable)!, out var error))
                {
                    log.Info($"Started {Path.GetFileName(launch.Executable)} as the signed-in user: {launch.Arguments}");
                    continue;
                }
                // Filee Rust notices it runs elevated and offers to restart without administrator rights.
                log.Warn($"Could not start {launch.Executable} as the signed-in user ({error}); starting it with Setup's rights");
            }
            Start(launch, log);
        }
    }

    private static void Start(LaunchRequest launch, SetupLog log)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(launch.Executable, launch.Arguments)
            {
                UseShellExecute = false,
                WorkingDirectory = Path.GetDirectoryName(launch.Executable)!,
            });
            log.Info($"Started {Path.GetFileName(launch.Executable)}: {launch.Arguments}");
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException)
        {
            log.Warn($"Could not start {launch.Executable}: {ex.Message}");
        }
    }

    /// <summary>Deletes <paramref name="folder"/> (the uninstaller's copy and files that were in use) once this process has ended.</summary>
    private static void CleanUpAfterExit(string folder, ISetupSystem system, SetupLog log)
    {
        if (!Directory.Exists(folder))
            return;
        foreach (var file in Directory.EnumerateFiles(folder, "*", SearchOption.AllDirectories))
            system.DeleteAtRestart(file);
        system.DeleteAtRestart(folder);
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(Path.Combine(Environment.SystemDirectory, "cmd.exe"),
                $"/d /c ping -n 4 127.0.0.1 >nul & rd /s /q \"{folder}\"")
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                WorkingDirectory = Environment.SystemDirectory,
            });
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException)
        {
            log.Warn($"Could not clean up {folder}: {ex.Message}");
        }
    }

    private static void TryDeleteFolder(string folder)
    {
        try
        {
            Directory.Delete(folder, recursive: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }

    /// <summary>A message box, unless /SUPPRESSMSGBOXES (the log has everything anyway).</summary>
    private static void Tell(SetupOptions options, string text, uint icon)
    {
        if (options.CanShowMessages)
            NativeMethods.MessageBox(0, text, "Filee Rust Setup", NativeMethods.MB_OK | icon | NativeMethods.MB_SETFOREGROUND);
    }
}

/// <summary>The real installer and uninstaller behind the window.</summary>
[System.Runtime.Versioning.SupportedOSPlatform("windows")]
internal sealed class SetupRunner(SetupContext context, SetupPayload? payload, PreviousInstall? previous,
    string? folder = null, InstallRecord? record = null, string? trash = null) : ISetupRunner
{
    public InstallResult Install(InstallChoices choices, IProgress<SetupProgress> progress) =>
        new Installer(context, payload ?? throw new InvalidOperationException("No payload.")).Run(choices, previous, progress);

    /// <summary>Lets the uninstaller in the install folder end before its file is deleted.</summary>
    public Action? ReleaseFirstPhase { get; init; }

    public Exception? Uninstall(IProgress<SetupProgress> progress)
    {
        try
        {
            new Uninstaller(context).Run(folder!, record!, trash!, progress, ReleaseFirstPhase);
            return null;
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            context.Log.Error($"Uninstall failed: {ex}");
            return ex;
        }
    }
}
