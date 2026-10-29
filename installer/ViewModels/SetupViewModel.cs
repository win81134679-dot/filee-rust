// One compact card in a few states: Welcome (the logo, one Install button and an Options sheet with the folder, the
// options, the engines and the language) → Progress → Done ("Finish": Filee Rust's settings window opens when it
// closes) or Failed ("Copy details"). An update keeps the folder and shows the options as they are now. The
// uninstaller uses the same card: Uninstall → Progress → Uninstalled.
// (Upstream's "What's new" slides, Explorer-menu, PATH and desktop-icon options and the "Launch" choice are gone.)

using System.Collections.ObjectModel;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Filee.Setup.Installation;
using Filee.Setup.Platform;

namespace Filee.Setup.ViewModels;

public enum SetupPage
{
    Welcome,
    Progress,
    Done,
    Failed,
    Uninstall,
    Uninstalled,
}

/// <summary>What the card does when the user goes ahead (the real installer, or a fake in tests).</summary>
public interface ISetupRunner
{
    InstallResult Install(InstallChoices choices, IProgress<SetupProgress> progress);

    /// <summary>Uninstalls; null when it worked, else the error.</summary>
    Exception? Uninstall(IProgress<SetupProgress> progress);
}

/// <summary>A language in the picker.</summary>
public sealed record LanguageItem(string Code, string Name)
{
    public override string ToString() => Name;
}

/// <summary>An engine in the Options sheet.</summary>
public sealed partial class EngineItem(string id, bool selected) : ObservableObject
{
    public string Id { get; } = id;

    [ObservableProperty] private bool _selected = selected;
    [ObservableProperty] private string _name = id;
}

public sealed partial class SetupViewModel : ObservableObject
{
    private readonly SetupTexts _texts;
    private readonly ISetupRunner _runner;
    private readonly PreviousInstall? _previous;
    private readonly InstallChoices _initial;
    private readonly SetupPaths _paths;
    private readonly bool _elevated;
    private readonly long _neededSpace;
    private bool _languagePicked;
    private InstallResult? _result;

    /// <param name="version">The Filee Rust version Setup installs (or the uninstaller removes).</param>
    /// <param name="initial">The choices from the command line and the defaults.</param>
    /// <param name="elevated">Setup runs with administrator rights already (an all-users install needs no new window).</param>
    /// <param name="uninstall">The uninstaller's card.</param>
    public SetupViewModel(SetupTexts texts, ISetupRunner runner, string version, InstallChoices initial, PreviousInstall? previous,
        SetupPaths paths, bool elevated, long neededSpace, bool uninstall = false, bool languagePicked = false)
    {
        _texts = texts;
        _runner = runner;
        _previous = previous;
        _initial = initial;
        _paths = paths;
        _elevated = elevated;
        _neededSpace = neededSpace;
        _languagePicked = languagePicked;
        Version = version;
        IsUninstall = uninstall;
        IsUpdate = previous is not null && !uninstall;
        Languages = SetupTexts.Languages.Select(l => new LanguageItem(l.Code, l.NativeName)).ToList();
        _selectedLanguage = Languages.FirstOrDefault(l => l.Code == texts.Language) ?? Languages[0];
        _folder = initial.Folder;
        _allUsers = initial.Scope == InstallScope.Machine;
        _startup = initial.Tasks.Startup;
        _sevenZip = initial.Tasks.SevenZip;
        _engineUpdate = initial.Tasks.EngineUpdate;
        Engines = new ObservableCollection<EngineItem>(
            SetupTasks.EngineIds.Select(id => new EngineItem(id, initial.Tasks.Engines.Contains(id, StringComparer.OrdinalIgnoreCase))));
        _page = uninstall ? SetupPage.Uninstall : SetupPage.Welcome;
        texts.LanguageChanged += (_, _) => RefreshTexts();
        RefreshTexts();
    }

    public string Version { get; }
    public bool IsUninstall { get; }
    public bool IsUpdate { get; }
    public bool IsFresh => !IsUpdate && !IsUninstall;
    public IReadOnlyList<LanguageItem> Languages { get; }
    public ObservableCollection<EngineItem> Engines { get; }

    /// <summary>/SILENT: the progress card only.</summary>
    public bool Silent { get; set; }

    /// <summary>Raised when the window should close (finished, cancelled).</summary>
    public event EventHandler? CloseRequested;

    /// <summary>Asks the view for a folder (the folder picker); null when cancelled.</summary>
    public Func<string, Task<string?>>? PickFolder { get; set; }

    /// <summary>Puts text on the clipboard (the view's).</summary>
    public Func<string, Task>? CopyText { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsWelcome), nameof(IsProgress), nameof(IsDone), nameof(IsFailed), nameof(IsUninstallPage),
        nameof(IsUninstalled), nameof(PrimaryText))]
    private SetupPage _page;

    public bool IsWelcome => Page == SetupPage.Welcome;
    public bool IsProgress => Page == SetupPage.Progress;
    public bool IsDone => Page == SetupPage.Done;
    public bool IsFailed => Page == SetupPage.Failed;
    public bool IsUninstallPage => Page == SetupPage.Uninstall;
    public bool IsUninstalled => Page == SetupPage.Uninstalled;

    [ObservableProperty] private bool _optionsOpen;
    [ObservableProperty] private LanguageItem _selectedLanguage;
    [ObservableProperty] private string _folder;
    [ObservableProperty] private bool _allUsers;
    [ObservableProperty] private bool _startup;
    [ObservableProperty] private bool _sevenZip;
    [ObservableProperty] private bool _engineUpdate;

    [ObservableProperty] private string _welcomeText = "";
    [ObservableProperty] private string _spaceText = "";
    [ObservableProperty] private string _statusText = "";
    [ObservableProperty] private double _percent;
    [ObservableProperty] private string _progressTitle = "";
    [ObservableProperty] private string _doneTitle = "";
    [ObservableProperty] private string _doneText = "";
    [ObservableProperty] private string _warningText = "";
    [ObservableProperty] private string _errorTitle = "";
    [ObservableProperty] private string _errorText = "";
    [ObservableProperty] private string _uninstallText = "";
    [ObservableProperty] private string _uninstalledText = "";
    [ObservableProperty] private string _logText = "";
    [ObservableProperty] private bool _copied;

    /// <summary>The big button of the Welcome card.</summary>
    public string PrimaryText => _texts[IsUpdate ? "installer.update" : "installer.install"];

    /// <summary>Exit code for Setup (Inno Setup's: 0 done, 2 cancelled before installing, 4 failed during installing).</summary>
    public int ExitCode { get; private set; } = 2;

    /// <summary>What to start when the window closes: Filee Rust's settings window (after a successful install).</summary>
    public LaunchRequest? Launch { get; private set; }

    /// <summary>The engine downloads to start when the window closes (null: none ticked).</summary>
    public LaunchRequest? EngineDownload { get; private set; }

    /// <summary>
    /// "All users" was ticked without administrator rights: the window closes and Program starts an elevated Setup with
    /// these choices (one UAC prompt) instead of installing from here.
    /// </summary>
    public InstallChoices? ElevateWith { get; private set; }

    public InstallResult? Result => _result;

    /// <summary>Setup's log file (named on the error card).</summary>
    public string? LogFile
    {
        get;
        set
        {
            field = value;
            RefreshTexts();
        }
    }

    partial void OnSelectedLanguageChanged(LanguageItem value)
    {
        if (value is null || value.Code == _texts.Language)
            return;
        _languagePicked = true;
        _texts.SetLanguage(value.Code);
    }

    partial void OnAllUsersChanged(bool value)
    {
        // The default folder follows the choice; a folder the user typed or picked stays.
        var from = _paths.DefaultInstallFolder(value ? InstallScope.User : InstallScope.Machine);
        if (Path.TrimEndingDirectorySeparator(Folder).Equals(from, StringComparison.OrdinalIgnoreCase))
            Folder = _paths.DefaultInstallFolder(value ? InstallScope.Machine : InstallScope.User);
    }

    /// <summary>A translated text for the current language.</summary>
    public string Text(string key) => _texts[key];

    /// <summary>Recomputes the texts with arguments (after a language change).</summary>
    private void RefreshTexts()
    {
        WelcomeText = IsUpdate
            ? _texts.Format("installer.welcome.update", _previous?.Version ?? "?", Version)
            : _texts.Format("installer.welcome.fresh", Version);
        SpaceText = _texts.Format("installer.options.space", Installer.FormatSize(_neededSpace));
        UninstallText = _texts.Format("installer.uninstall.text", Version);
        LogText = LogFile is null ? "" : _texts.Format("installer.error.log", LogFile);
        foreach (var engine in Engines)
            engine.Name = _texts[$"installer.engine.{engine.Id}"];
        ErrorTitle = _texts[IsUninstall ? "installer.error.uninstall_title" : "installer.error.title"];
        ProgressTitle = _texts[IsUninstall ? "installer.uninstall.progress" : IsUpdate ? "installer.progress.update" : "installer.progress.install"];
        DoneTitle = _texts["installer.done.title"];
        OnPropertyChanged(nameof(PrimaryText));
    }

    [RelayCommand]
    private void ToggleOptions() => OptionsOpen = !OptionsOpen;

    [RelayCommand]
    private async Task BrowseAsync()
    {
        if (PickFolder is null)
            return;
        if (await PickFolder(Folder) is { } picked)
        {
            // A folder of its own, like Inno Setup: picking "D:\Apps" installs into "D:\Apps\Filee Rust".
            Folder = Path.GetFileName(Path.TrimEndingDirectorySeparator(picked)).Equals("Filee Rust", StringComparison.OrdinalIgnoreCase)
                ? picked
                : Path.Combine(picked, "Filee Rust");
        }
    }

    /// <summary>The window's close button and Escape: closes the Options sheet first, then cancels (before installing).</summary>
    [RelayCommand]
    private void Cancel()
    {
        if (OptionsOpen)
        {
            OptionsOpen = false;
            return;
        }
        if (IsProgress)
            return;
        WindowClosing();
        CloseRequested?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>The window is closing: before installing it is a cancel; after, nothing more is changed.</summary>
    public void WindowClosing()
    {
        if (Page is SetupPage.Welcome or SetupPage.Uninstall)
            ExitCode = 2;
    }

    [RelayCommand]
    private Task Install() => InstallAsync();

    [RelayCommand]
    private Task Uninstall() => UninstallAsync();

    [RelayCommand]
    private void Close() => CloseRequested?.Invoke(this, EventArgs.Empty);

    [RelayCommand]
    private async Task CopyDetailsAsync()
    {
        if (CopyText is null)
            return;
        await CopyText(ErrorText + Environment.NewLine + Environment.NewLine + LogText);
        Copied = true;
    }

    /// <summary>The choices as they stand in the Options sheet.</summary>
    public InstallChoices Choices() => _initial with
    {
        Folder = Folder,
        Scope = IsFresh ? (AllUsers ? InstallScope.Machine : InstallScope.User) : _initial.Scope,
        Tasks = new SetupTasks(Startup, SevenZip, EngineUpdate, Engines.Where(e => e.Selected).Select(e => e.Id).ToList()),
        Language = _texts.Language,
    };

    /// <summary>Installs with the current choices (also what /SILENT does right away).</summary>
    public async Task InstallAsync()
    {
        if (Page != SetupPage.Welcome)
            return;
        RefreshTexts();
        var choices = Choices();
        if (NeedsElevation(choices))
        {
            ElevateWith = choices;
            OptionsOpen = false;
            CloseRequested?.Invoke(this, EventArgs.Empty);
            return;
        }
        OptionsOpen = false;
        Page = SetupPage.Progress;
        Percent = 0;
        var progress = new SyncProgress<SetupProgress>(p => Dispatcher.UIThread.Post(() => ShowProgress(p)));
        _result = await Task.Run(() => _runner.Install(choices, progress));
        if (_result.Succeeded)
        {
            ExitCode = 0;
            Percent = 100;
            Launch = _result.Launch;
            EngineDownload = _result.EngineDownload;
            WarningText = string.Join(Environment.NewLine + Environment.NewLine, _result.Warnings.Select(WarningMessage));
            // With a warning to read, the card leaves out how to use Filee Rust.
            DoneText = string.Join(Environment.NewLine + Environment.NewLine, new[]
            {
                IsUpdate ? _texts["installer.done.updated"] : WarningText.Length > 0 ? "" : _texts["installer.done.text"],
                _result.EngineDownload is not null ? _texts["installer.done.engines"] : "",
            }.Where(t => t.Length > 0));
            Page = SetupPage.Done;
        }
        else
        {
            ExitCode = 4;
            ErrorText = ErrorMessage(_result.Error!) + Environment.NewLine + Environment.NewLine
                        + _texts[_result.RolledBack ? IsUpdate ? "installer.error.rolled_back" : "installer.error.undone" : "installer.error.nothing_changed"];
            Page = SetupPage.Failed;
        }
    }

    private bool NeedsElevation(InstallChoices choices) => choices.Scope == InstallScope.Machine && !_elevated;

    /// <summary>Uninstalls (also what a silent uninstall does right away).</summary>
    public async Task UninstallAsync()
    {
        if (Page != SetupPage.Uninstall)
            return;
        RefreshTexts();
        Page = SetupPage.Progress;
        Percent = 0;
        var progress = new SyncProgress<SetupProgress>(p => Dispatcher.UIThread.Post(() => ShowProgress(p)));
        var error = await Task.Run(() => _runner.Uninstall(progress));
        if (error is null)
        {
            ExitCode = 0;
            Percent = 100;
            UninstalledText = _texts["installer.uninstall.done"] + Environment.NewLine + Environment.NewLine + _texts["installer.uninstall.settings_kept"];
            Page = SetupPage.Uninstalled;
        }
        else
        {
            ExitCode = 1;
            ErrorText = ErrorMessage(error);
            Page = SetupPage.Failed;
        }
    }

    private void ShowProgress(SetupProgress progress)
    {
        Percent = Math.Round(progress.Fraction * 100, 1);
        StatusText = _texts[progress.Step switch
        {
            SetupStep.Verifying => "installer.status.verifying",
            SetupStep.Closing => "installer.status.closing",
            SetupStep.RemovingOld => "installer.status.removing_old",
            SetupStep.Copying => "installer.status.copying",
            SetupStep.RollingBack => "installer.status.rolling_back",
            SetupStep.Removing => "installer.status.removing",
            _ => "installer.status.finishing",
        }];
    }

    /// <summary>A warning in the UI language.</summary>
    public string WarningMessage(SetupWarning warning) => SetupMessages.Warning(_texts, warning);

    /// <summary>An error in the UI language.</summary>
    public string ErrorMessage(Exception error) => SetupMessages.Error(_texts, error);
}

/// <summary>Warnings and errors in the UI language.</summary>
public static class SetupMessages
{
    public static string Warning(SetupTexts texts, SetupWarning warning) => warning switch
    {
        RuntimeWarning runtime => texts.Format("installer.warning.runtime",
            string.Join(", ", runtime.Missing.Select(m => Path.GetFileName(m.Program)).Distinct(StringComparer.OrdinalIgnoreCase)),
            string.Join(", ", runtime.Missing.Select(m => m.Dll).Distinct(StringComparer.OrdinalIgnoreCase)),
            SetupNames.VcRedistUrl),
        _ => warning.ToString(),
    };

    /// <summary>The technical message only for errors nobody expected.</summary>
    public static string Error(SetupTexts texts, Exception error) => error switch
    {
        SetupException known => texts.Format(known.TextKey, [.. known.Arguments]),
        InvalidDataException => texts["installer.error.damaged"],
        UnauthorizedAccessException or IOException => texts.Format("installer.error.files", error.Message),
        _ => texts.Format("installer.error.unexpected", error.Message),
    };
}
