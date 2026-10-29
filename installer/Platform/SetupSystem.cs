// What Setup does to Windows besides files and the registry: shortcuts, running the product's own commands (--quit),
// closing programs. (The fork has no Explorer menu package and no PATH entry: filee-rust.exe writes its own verb.) Tests replace it with a fake;
// a /SANDBOX run with SandboxSystem, which leaves the PC alone.

using System.Diagnostics;
using System.Runtime.Versioning;

namespace Filee.Setup.Platform;

/// <summary>A process started from a file in the install folder.</summary>
public sealed record RunningProgram(int Id, string Path);

public interface ISetupSystem
{
    void CreateShortcut(string path, string target, string workingFolder, string description, string arguments = "", string? icon = null);

    /// <summary>Runs a program without a window and waits; the exit code, or -1 when it failed or took too long.</summary>
    int RunAndWait(string executable, string arguments, TimeSpan timeout);

    IReadOnlyList<RunningProgram> ProgramsIn(string folder);

    void Kill(int processId);

    /// <summary>Has Windows delete a file or an empty folder at the next restart (what is still in use now).</summary>
    void DeleteAtRestart(string path);
}

/// <summary>The real thing.</summary>
[SupportedOSPlatform("windows")]
public class WindowsSetupSystem(SetupLog log) : ISetupSystem
{
    protected SetupLog Log { get; } = log;

    public virtual void CreateShortcut(string path, string target, string workingFolder, string description, string arguments = "", string? icon = null) =>
        ShellLink.Create(path, target, workingFolder, description, arguments, icon);

    public virtual int RunAndWait(string executable, string arguments, TimeSpan timeout)
    {
        // Something that isn't a program (a damaged or half-written Filee.exe) makes Windows show "This app can't run
        // on your PC" and wait for an answer, even with Setup's error mode: never start one.
        if (!IsProgram(executable))
        {
            Log.Warn($"Not running {executable}: it is not a Windows program");
            return -1;
        }
        try
        {
            using var process = Process.Start(CreateStartInfo(executable, arguments));
            if (process is null)
                return -1;
            if (process.WaitForExit(timeout))
                return process.ExitCode;
            Log.Warn($"{Path.GetFileName(executable)} {arguments} did not finish within {timeout.TotalSeconds:0} s");
            process.Kill();
            return -1;
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            Log.Warn($"Could not run {executable} {arguments}: {ex.Message}");
            return -1;
        }
    }

    private static bool IsProgram(string path)
    {
        try
        {
            using var file = File.OpenRead(path);
            Span<byte> header = stackalloc byte[2];
            return file.Read(header) == 2 && header[0] == 'M' && header[1] == 'Z';
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    protected virtual ProcessStartInfo CreateStartInfo(string executable, string arguments) =>
        new(executable, arguments)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            WorkingDirectory = Path.GetDirectoryName(executable) ?? "",
        };

    public virtual IReadOnlyList<RunningProgram> ProgramsIn(string folder)
    {
        var prefix = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder)) + Path.DirectorySeparatorChar;
        var found = new List<RunningProgram>();
        foreach (var process in Process.GetProcesses())
        {
            using (process)
            {
                if (process.Id == Environment.ProcessId)
                    continue;
                var path = NativeMethods.ImagePathOf(process.Id);
                if (path is not null && path.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
                    found.Add(new RunningProgram(process.Id, path));
            }
        }
        return found;
    }

    public virtual void Kill(int processId)
    {
        try
        {
            using var process = Process.GetProcessById(processId);
            process.Kill(entireProcessTree: false);
            process.WaitForExit(TimeSpan.FromSeconds(10));
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            // Ended meanwhile, or not ours to end: the file swap copes with files in use.
        }
    }

    public virtual void DeleteAtRestart(string path)
    {
        if (!NativeMethods.MoveFileEx(path, null, NativeMethods.MOVEFILE_DELAY_UNTIL_REBOOT))
            Log.Warn($"Could not schedule {path} for deletion at the next restart");
    }

    private static void TryDelete(string file)
    {
        try
        {
            File.Delete(file);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }
}

/// <summary>
/// /SANDBOX: nothing scheduled for the next restart, and the product's commands run with FILEE_DATA_DIR in the sandbox.
/// </summary>
[SupportedOSPlatform("windows")]
public sealed class SandboxSetupSystem(SetupLog log, string dataFolder) : WindowsSetupSystem(log)
{
    public override void DeleteAtRestart(string path) => Log.Info($"Sandbox: would delete {path} at the next restart");

    protected override ProcessStartInfo CreateStartInfo(string executable, string arguments)
    {
        var info = base.CreateStartInfo(executable, arguments);
        info.Environment["FILEE_DATA_DIR"] = dataFolder;
        return info;
    }
}
