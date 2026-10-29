// The options ("tasks") of Setup, with the names the Inno Setup installer used for them (packaging/FileeRust.iss):
// /TASKS= and /MERGETASKS= take these names, as Inno does.
//
//   startup                 start Filee Rust when the user signs in (HKCU Run value FileeRust)
//   sevenzip                the bundled 7-Zip (app\engines\7zip)
//   engineupdate            the weekly engine update (unticked: engine-update.disabled)
//   engines                 parent of the downloads below
//   engines\libreoffice … \calibre
//                           engines Filee's own downloader fetches after the install

namespace Filee.Setup.Installation;

/// <summary>What Setup sets up besides copying the files.</summary>
/// <param name="Startup">Start Filee Rust at sign-in.</param>
/// <param name="SevenZip">Keep the bundled 7-Zip.</param>
/// <param name="EngineUpdate">Update the engines weekly.</param>
/// <param name="Engines">Ids of the engines to download after the install (a subset of <see cref="EngineIds"/>).</param>
public sealed record SetupTasks(bool Startup, bool SevenZip, bool EngineUpdate, IReadOnlyList<string> Engines)
{
    public const string StartupName = "startup";
    public const string SevenZipName = "sevenzip";
    public const string EngineUpdateName = "engineupdate";
    public const string EnginesName = "engines";
    private const string EnginePrefix = EnginesName + "\\";

    /// <summary>The engines Setup offers, in the order of the Options sheet (ids of Filee's downloader).</summary>
    public static readonly IReadOnlyList<string> EngineIds = ["libreoffice", "tesseract", "pandoc", "ghostscript", "ffmpeg", "calibre"];

    /// <summary>Every task, parents before their children.</summary>
    public static readonly IReadOnlyList<string> Names =
        [StartupName, SevenZipName, EngineUpdateName, EnginesName, .. EngineIds.Select(id => EnginePrefix + id)];

    /// <summary>A fresh install ticks everything.</summary>
    public static SetupTasks Defaults { get; } = new(true, true, true, EngineIds);

    /// <summary>The same options without any download.</summary>
    public SetupTasks WithoutEngines() => this with { Engines = [] };

    /// <summary>
    /// /TASKS (only the listed tasks) and /MERGETASKS (changes to <paramref name="defaults"/>), as Inno Setup reads
    /// them: "!name" unticks, "*name" ticks the task and its children. A ticked child ticks its parent.
    /// </summary>
    public static SetupTasks FromCommandLine(string? tasks, string? mergeTasks, SetupTasks? defaults = null)
    {
        var selected = tasks is null ? (defaults ?? Defaults).ToSet() : new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        Apply(selected, tasks);
        Apply(selected, mergeTasks);
        return FromSet(selected);
    }

    /// <summary>The tasks in Inno Setup's registry format ("startup,sevenzip,engines,engines\ffmpeg").</summary>
    public string ToInnoString() => string.Join(',', Names.Where(ToSet().Contains));

    /// <summary>The task names Setup doesn't know in <paramref name="list"/> (logged).</summary>
    public static IEnumerable<string> UnknownNames(string? list) =>
        Split(list).Select(n => n.TrimStart('!', '*')).Where(n => !Names.Contains(n, StringComparer.OrdinalIgnoreCase));

    private static void Apply(HashSet<string> selected, string? list)
    {
        foreach (var entry in Split(list))
        {
            var name = entry.TrimStart('!', '*');
            var known = Names.FirstOrDefault(n => n.Equals(name, StringComparison.OrdinalIgnoreCase));
            if (known is null)
                continue;
            name = known;
            var children = Names.Where(n => n.StartsWith(name + "\\", StringComparison.OrdinalIgnoreCase)).ToList();
            if (entry.StartsWith('!'))
            {
                selected.Remove(name);
                // An unticked parent unticks its children (they can't be on without it).
                children.ForEach(c => selected.Remove(c));
                continue;
            }
            selected.Add(name);
            if (entry.StartsWith('*'))
                children.ForEach(c => selected.Add(c));
            for (var slash = name.LastIndexOf('\\'); slash > 0; slash = name.LastIndexOf('\\', slash - 1))
                selected.Add(name[..slash]);
        }
    }

    private static IEnumerable<string> Split(string? list) =>
        (list ?? "").Split([',', ';', ' '], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);

    private HashSet<string> ToSet()
    {
        var set = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        if (Startup)
            set.Add(StartupName);
        if (SevenZip)
            set.Add(SevenZipName);
        if (EngineUpdate)
            set.Add(EngineUpdateName);
        foreach (var id in EngineIds.Where(id => Engines.Contains(id, StringComparer.OrdinalIgnoreCase)))
        {
            set.Add(EnginesName);
            set.Add(EnginePrefix + id);
        }
        return set;
    }

    private static SetupTasks FromSet(HashSet<string> set) => new(
        set.Contains(StartupName),
        set.Contains(SevenZipName),
        set.Contains(EngineUpdateName),
        set.Contains(EnginesName) ? EngineIds.Where(id => set.Contains(EnginePrefix + id)).ToList() : []);
}
