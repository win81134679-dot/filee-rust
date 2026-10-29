using Filee.Setup;
using Filee.Setup.Installation;
using Xunit;

namespace FileeRust.Setup.Tests;

/// <summary>The command line: the switches the installed Filee Rust passes for a self-update, Inno's others, ours.</summary>
public class SetupOptionsTests
{
    // What shell\src\core\self_update.rs runs (it cannot change once shipped).
    private static readonly string[] SelfUpdateArgs =
        ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/SELFUPDATE=1", @"/LOG=C:\Temp\update.log"];

    [Fact]
    public void SelfUpdateCommandLineIsAcceptedInFull()
    {
        var o = SetupOptions.Parse(SelfUpdateArgs);
        Assert.Equal(SetupUi.VerySilent, o.Ui);
        Assert.True(o.IsSilent);
        Assert.True(o.SuppressMessageBoxes);
        Assert.True(o.SelfUpdate);
        Assert.Equal(@"C:\Temp\update.log", o.LogFile);
        // Per user without being asked: the self-update only exists for current-user installs.
        Assert.Equal(InstallScope.User, o.Scope);
        Assert.False(o.ScopeGiven);
        Assert.Contains("/SP-", o.Ignored);
        Assert.Contains("/NORESTART", o.Ignored);
        Assert.Empty(o.Unknown);
        Assert.False(o.LaunchAfter);
    }

    [Theory]
    [InlineData("/SELFUPDATE=1", true)]
    [InlineData("/selfupdate=1", true)]
    [InlineData("/SELFUPDATE", true)]
    [InlineData("/SELFUPDATE=0", false)]
    [InlineData("/SELFUPDATE=2", false)]
    public void SelfUpdateOnlyForOne(string arg, bool expected) =>
        Assert.Equal(expected, SetupOptions.Parse(["/VERYSILENT", arg]).SelfUpdate);

    [Fact]
    public void OtherSilentInstallsAreNotSelfUpdates()
    {
        var o = SetupOptions.Parse(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]);
        Assert.False(o.SelfUpdate);
        Assert.False(o.LaunchAfter);
        Assert.Equal(SetupUi.VerySilent, o.Ui);
    }

    [Fact]
    public void ScopeSwitches()
    {
        Assert.Equal(InstallScope.User, SetupOptions.Parse([]).Scope);
        var all = SetupOptions.Parse(["/ALLUSERS"]);
        Assert.Equal(InstallScope.Machine, all.Scope);
        Assert.True(all.ScopeGiven);
        var user = SetupOptions.Parse(["/ALLUSERS", "/CURRENTUSER"]);
        Assert.Equal(InstallScope.User, user.Scope);
        Assert.True(user.ScopeGiven);
    }

    [Fact]
    public void InnoSwitchesKeepWorking()
    {
        var o = SetupOptions.Parse(["/SILENT", @"/DIR=""D:\Apps\Filee Rust""", "/LANG=zhtw", @"/TASKS=""startup,!sevenzip""",
            "/MERGETASKS=engines", "/NOICONS", "/NOCLOSEAPPLICATIONS", "/COMPONENTS=x", "/NOCANCEL", "/nope"]);
        Assert.Equal(SetupUi.Silent, o.Ui);
        Assert.Equal(@"D:\Apps\Filee Rust", o.Directory);
        Assert.Equal("zhtw", o.Language);
        Assert.Equal("startup,!sevenzip", o.Tasks);
        Assert.Equal("engines", o.MergeTasks);
        Assert.True(o.NoIcons);
        Assert.False(o.ForceCloseApplications);
        Assert.Contains("/COMPONENTS=x", o.Ignored);
        Assert.Contains("/NOCANCEL", o.Ignored);
        Assert.Equal(["/nope"], o.Unknown);
    }

    [Fact]
    public void VerySilentWinsOverSilent()
    {
        Assert.Equal(SetupUi.VerySilent, SetupOptions.Parse(["/VERYSILENT", "/SILENT"]).Ui);
        Assert.Equal(SetupUi.VerySilent, SetupOptions.Parse(["/SILENT", "/VERYSILENT"]).Ui);
    }

    [Fact]
    public void OurInternalSwitches()
    {
        var o = SetupOptions.Parse(["/LAUNCHAFTER", @"/LAUNCHFILE=C:\t\x.launch", "/NOLAUNCH", "/FIRSTPHASE=12", "/UNINSTALL", @"/SANDBOX=C:\sb"]);
        Assert.True(o.LaunchAfter);
        Assert.Equal(@"C:\t\x.launch", o.LaunchFile);
        Assert.True(o.NoLaunch);
        Assert.Equal(12, o.FirstPhase);
        Assert.True(o.Uninstall);
        Assert.Equal(@"C:\sb", o.Sandbox);
    }

    [Fact]
    public void PortableIsGone()
    {
        Assert.Equal(["/PORTABLE=1"], SetupOptions.Parse(["/PORTABLE=1"]).Unknown);
    }

    [Fact]
    public void QuoteAndJoinRoundTripThroughTheCommandLineRules()
    {
        Assert.Equal(@"""C:\a b\c.exe""", SetupOptions.Quote(@"C:\a b\c.exe"));
        Assert.Equal("/ALLUSERS", SetupOptions.Quote("/ALLUSERS"));
    }
}

/// <summary>The options ("tasks") with Inno Setup's names.</summary>
public class SetupTasksTests
{
    [Fact]
    public void EverythingIsTickedByDefault()
    {
        var t = SetupTasks.FromCommandLine(null, null);
        Assert.True(t.Startup);
        Assert.True(t.SevenZip);
        Assert.True(t.EngineUpdate);
        Assert.Equal(["libreoffice", "tesseract", "pandoc", "ghostscript", "ffmpeg", "calibre"], t.Engines);
    }

    [Fact]
    public void TasksListsExactlyTheseAndAChildTicksItsParent()
    {
        var t = SetupTasks.FromCommandLine(@"startup,engines\ffmpeg", null);
        Assert.True(t.Startup);
        Assert.False(t.SevenZip);
        Assert.False(t.EngineUpdate);
        Assert.Equal(["ffmpeg"], t.Engines);
        Assert.Equal(@"startup,engines,engines\ffmpeg", t.ToInnoString());
    }

    [Fact]
    public void MergeTasksChangesTheDefaults()
    {
        var t = SetupTasks.FromCommandLine(null, @"!startup,!engines\calibre,!engineupdate");
        Assert.False(t.Startup);
        Assert.False(t.EngineUpdate);
        Assert.True(t.SevenZip);
        Assert.DoesNotContain("calibre", t.Engines);
        Assert.Equal(5, t.Engines.Count);
    }

    [Fact]
    public void UntickingTheParentUnticksAllEngines()
    {
        Assert.Empty(SetupTasks.FromCommandLine(null, "!engines").Engines);
        Assert.Equal(6, SetupTasks.FromCommandLine("*engines", null).Engines.Count);
    }

    [Fact]
    public void UpdateDefaultsComeFromTheMachine()
    {
        var current = new SetupTasks(false, true, false, []);
        var t = SetupTasks.FromCommandLine(null, null, current);
        Assert.False(t.Startup);
        Assert.True(t.SevenZip);
        Assert.False(t.EngineUpdate);
        Assert.Empty(t.Engines);
    }

    [Fact]
    public void UnknownNamesAreReportedNotFatal()
    {
        Assert.Equal(["contextmenu", "cli"], SetupTasks.UnknownNames("startup,!contextmenu,*cli,engines\\ffmpeg"));
        Assert.True(SetupTasks.FromCommandLine("contextmenu,startup", null).Startup);
    }

    [Fact]
    public void UpstreamsExplorerAndPathOptionsAreNotOffered()
    {
        Assert.DoesNotContain("contextmenu", SetupTasks.Names);
        Assert.DoesNotContain("cli", SetupTasks.Names);
        Assert.DoesNotContain("desktopicon", SetupTasks.Names);
    }

    [Fact]
    public void ToInnoStringRoundTrips()
    {
        var all = SetupTasks.Defaults;
        Assert.Equal(all.Engines, SetupTasks.FromCommandLine(all.ToInnoString(), null).Engines);
        Assert.Equal(all.ToInnoString(), SetupTasks.FromCommandLine(all.ToInnoString(), null).ToInnoString());
    }
}
