// Names Setup shares with the Inno Setup installer it replaced (packaging/FileeRust.iss), the product and Windows.
// Changing one breaks upgrades in place and self-update.

namespace Filee.Setup.Installation;

public static class SetupNames
{
    /// <summary>
    /// Inno Setup's AppId of Filee Rust. Apps &amp; Features lists it under the uninstall key "{id}_is1"; Setup keeps
    /// that key, so an Inno-installed Filee Rust is upgraded in place and one entry stays.
    /// </summary>
    public const string AppGuid = "6B0F3C5E-2D1A-4E57-9E2B-F11EE0717E01";

    public const string UninstallKey = @"Software\Microsoft\Windows\CurrentVersion\Uninstall\{" + AppGuid + "}_is1";

    /// <summary>
    /// The original Filee's own AppId. Only ever read (its install folder, to ask it to quit): Setup must never
    /// write or delete this key.
    /// </summary>
    public const string OriginalFileeKey = @"Software\Microsoft\Windows\CurrentVersion\Uninstall\{C8686E06-8327-40A2-B0F7-3DEDD61D9248}_is1";

    /// <summary>The original Filee's program, in the folder its key names.</summary>
    public const string OriginalFileeExe = "Filee.exe";

    public const string AppName = "Filee Rust";
    public const string Publisher = "Filee Rust (based on Filee by Filee contributors, MIT)";
    public const string AppUrl = "https://github.com/KnifeLemon/Filee";

    /// <summary>The resident shell: the program users start.</summary>
    public const string AppExe = "filee-rust.exe";

    /// <summary>Filee Lite, the name before 2026-10-29: removed on upgrade.</summary>
    public const string LegacyExe = "filee-lite.exe";
    public const string LegacyShortcut = "Filee Lite.lnk";
    public const string LegacyRunValue = "FileeLite";

    /// <summary>The icon of the uninstall entry and the shortcut (also installed).</summary>
    public const string IconFile = "filee.ico";

    /// <summary>The uninstaller next to filee-rust.exe.</summary>
    public const string UninstallerExe = "uninstall.exe";

    /// <summary>The list of installed files next to filee-rust.exe: what the uninstaller removes and an update replaces.</summary>
    public const string InstallRecordFile = "uninstall.json";

    /// <summary>Inno Setup's uninstaller files, removed when Setup takes over an Inno installation.</summary>
    public static readonly IReadOnlyList<string> InnoUninstallerFiles = ["unins000.exe", "unins000.dat", "unins000.msg"];

    /// <summary>Start menu shortcut name.</summary>
    public const string ShortcutName = "Filee Rust.lnk";

    /// <summary>What the shortcut starts filee-rust.exe with.</summary>
    public const string ShortcutArguments = "--settings";

    /// <summary>Start at sign-in: HKCU Run value (the shell never writes it itself; antivirus may block that).</summary>
    public const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    public const string RunValue = "FileeRust";
    public const string RunArguments = "--background";

    /// <summary>The Explorer verb filee-rust.exe writes itself (settings.json contextMenuEnabled); removed on uninstall.</summary>
    public const string ExplorerVerbKey = @"Software\Classes\*\shell\FileeLite";

    /// <summary>filee-rust.exe skips the weekly engine update while this file exists (core::engine_update).</summary>
    public const string EngineUpdateOffFile = "engine-update.disabled";

    /// <summary>The bundled 7-Zip, relative to the install folder (a task: unticked → not installed, removed).</summary>
    public const string SevenZipFolder = @"app\engines\7zip";

    /// <summary>The program that downloads the engines, relative to the install folder.</summary>
    public const string HostedExe = @"app\Filee.exe";

    /// <summary>Registry values Setup adds to the uninstall key (Inno Setup kept the same in "Inno Setup: …").</summary>
    public const string TasksValue = "Filee Setup: Tasks";
    public const string LanguageValue = "Filee Setup: Language";
    public const string ScopeValue = "Filee Setup: Scope";
    public const string InnoAppPathValue = "Inno Setup: App Path";
    public const string InnoVersionValue = "Inno Setup: Setup Version";
    public const string InnoLanguageValue = "Inno Setup: Language";

    /// <summary>Windows 10 1809: the oldest Windows Filee runs on (Inno Setup's MinVersion).</summary>
    public const int MinimumBuild = 17763;

    /// <summary>Where Microsoft offers the Visual C++ Redistributable (x64).</summary>
    public const string VcRedistUrl = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
}
