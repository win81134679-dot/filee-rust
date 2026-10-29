// Makes FileeRust-<version>-Setup.exe: appends the staged product (build/stage: filee-rust.exe, app\, i18n\, licenses\ …)
// to the published Setup program as an LZMA payload with a SHA-256 manifest (see Payload/SetupPayload.cs).
//
// Adapted from upstream Filee's build/tools/make-setup.cs (MIT, Copyright (c) Filee contributors). Differences:
// the product is checked by filee-rust.exe (not Filee.exe), and no engines are written into the manifest: the Options sheet
// offers the six engines of packaging/FileeRust.iss itself (SetupTasks.EngineIds), so this needs no Filee.Engines.
//
// Run by 工具/build_release.py:
//   dotnet run installer/tools/make-setup.cs -- <Filee.Setup.exe> <stage folder> <version> <output .exe> [--fast]
// LZMA with a 48 MB dictionary, programs through the x86 branch filter; --fast uses a small dictionary, for checks.
#:project ../FileeRust.Setup.csproj
#:property PublishAot=false

using System.Diagnostics;
using Filee.Setup.Installation;
using Filee.Setup.Payload;

var fast = args.Contains("--fast");
var positional = args.Where(a => a != "--fast").ToArray();
if (positional.Length != 4)
{
    Console.Error.WriteLine("usage: dotnet run installer/tools/make-setup.cs -- <Filee.Setup.exe> <stage folder> <version> <output .exe> [--fast]");
    return 2;
}
var (program, app, version, output) = (positional[0], positional[1], positional[2], positional[3]);
if (!File.Exists(Path.Combine(app, SetupNames.AppExe)) || !File.Exists(Path.Combine(app, SetupNames.HostedExe)))
{
    Console.Error.WriteLine($"{SetupNames.AppExe} or {SetupNames.HostedExe} is missing in {app}");
    return 1;
}

var watch = Stopwatch.StartNew();
var manifest = SetupPayload.Write(program, app, Path.GetFullPath(output), version, [], "", fast, Console.WriteLine);
var size = new FileInfo(output).Length;
Console.WriteLine($"{manifest.Files.Count} files, {manifest.TotalSize / 1_000_000} MB -> {output} ({size / 1_000_000} MB) in {watch.Elapsed.TotalSeconds:0} s");
return 0;
