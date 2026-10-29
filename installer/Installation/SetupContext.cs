// What an install or uninstall works with, and what it reports back.

using Filee.Setup.Platform;

namespace Filee.Setup.Installation;

/// <param name="Sandbox">A /SANDBOX run: Filee is never started.</param>
public sealed record SetupContext(ISetupRegistry Registry, ISetupSystem System, SetupPaths Paths, SetupLog Log, bool Sandbox = false);

/// <summary>What the user chose (or the command line said).</summary>
/// <param name="Folder">The install folder.</param>
/// <param name="Tasks">The options, including the engines to download after the install.</param>
/// <param name="Language">Setup's language (kept in the uninstall entry for the uninstaller).</param>
/// <param name="NoIcons">/NOICONS: no Start menu shortcut.</param>
/// <param name="ForceClose">Programs still running from the install folder after "--quit" are ended.</param>
public sealed record InstallChoices(
    string Folder,
    InstallScope Scope,
    SetupTasks Tasks,
    string Language,
    bool NoIcons = false,
    bool ForceClose = true);

/// <summary>What Setup is doing (the progress page shows it in the UI language).</summary>
public enum SetupStep
{
    Verifying,
    Closing,
    RemovingOld,
    Copying,
    Finishing,
    RollingBack,
    Removing,
}

public sealed record SetupProgress(SetupStep Step, double Fraction);

/// <summary>Something that didn't work but leaves a working Filee Rust.</summary>
public abstract record SetupWarning;

/// <summary>Programs that need a Microsoft Visual C++ runtime this PC doesn't have.</summary>
public sealed record RuntimeWarning(IReadOnlyList<MissingRuntime> Missing) : SetupWarning;

/// <summary>Start Filee Rust as the signed-in user with these arguments.</summary>
public sealed record LaunchRequest(string Executable, string Arguments);

/// <summary>A problem with a message for the user (the key of a translated text and its arguments).</summary>
public sealed class SetupException(string textKey, string message, params string[] arguments) : Exception(message)
{
    public string TextKey { get; } = textKey;
    public IReadOnlyList<string> Arguments { get; } = arguments;
}

/// <param name="Launch">filee-rust.exe --settings: what an interactive install starts at the end.</param>
/// <param name="EngineDownload">Filee.exe --hosted --install-engines=…: started (unelevated, not awaited) by an interactive install.</param>
/// <param name="Error">Why it failed; null when it worked.</param>
/// <param name="RolledBack">The install folder is as it was before (the previous Filee Rust still works).</param>
public sealed record InstallResult(LaunchRequest? Launch, IReadOnlyList<SetupWarning> Warnings, Exception? Error = null, bool RolledBack = false,
    LaunchRequest? EngineDownload = null)
{
    public bool Succeeded => Error is null;
}
