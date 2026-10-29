"""Build a Filee Rust installer for the upstream version checked out in upstream/.

Steps (each one stops the build with a message when it fails):
  1. patch   upstream/ is at a tag and patches/hosted.patch is applied (3-way; a conflict stops here)
  2. fonts   upstream's embedded fonts (fetch_upstream_fonts.py: parsed from its build script, SHA-256 checked)
  3. dotnet  publish the patched Filee.App (self-contained, ReadyToRun) → build/upstream-publish
  4. base    the official win-Portable.zip of that tag (SHA256SUMS checked) → cache/upstream-releases/<tag>
  5. stage   build/stage: portable + patched Filee.dll and Filee.Engines.dll (+pdb/xml), i18n, icon, licenses, README.txt
  6. cargo   shell/ release build → build/stage/filee-rust.exe (cargo test first)
  7. check   filee-cli version, filee-rust --selftest, one real conversion with the staged CLI
  8. iscc    packaging/FileeRust.iss → dist/FileeRust-<ver>-Setup.exe

Usage: python build_release.py [--lite-rev N] [--skip-tests]
Version: <upstream version>-rust.<N> (N from --lite-rev, default LITE_REV below); file version <upstream>.<N>.
Raise LITE_REV whenever a changed build goes to other computers, so its installer has a new name.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LITE_REV = 5  # 2026-10-29: self-update (4: renamed to Filee Rust, 3: PDF Office group, 2: empty-donut fix)
UPSTREAM = ROOT / "upstream"
PUBLIC_KEY = ROOT / "packaging" / "update.pub"
PATCH = ROOT / "patches" / "hosted.patch"
BUILD = ROOT / "build"
STAGE = BUILD / "stage"
PUBLISH = BUILD / "upstream-publish"
CACHE = ROOT / "cache" / "upstream-releases"
DIST = ROOT / "dist"
SHELL = ROOT / "shell"
PY = sys.executable
DOTNET = Path(r"C:\Program Files\dotnet\dotnet.exe")
ISCC_CANDIDATES = [
    Path(os.environ.get("LOCALAPPDATA", "")) / "Programs" / "Inno Setup 6" / "ISCC.exe",
    Path(r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe"),
    Path(r"C:\Program Files\Inno Setup 6\ISCC.exe"),
]
REPO = "KnifeLemon/Filee"


class Fail(Exception):
    pass


def step(name: str) -> None:
    print(f"\n=== {name}", flush=True)


def run(cmd: list, cwd: Path | None = None, env: dict | None = None, quiet: bool = False) -> str:
    out = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if out.returncode != 0:
        tail = "\n".join((out.stdout + out.stderr).strip().splitlines()[-30:])
        raise Fail(f"{' '.join(map(str, cmd))} → exit {out.returncode}\n{tail}")
    if not quiet and out.stdout.strip():
        print("\n".join(out.stdout.strip().splitlines()[-8:]))
    return out.stdout


def git(*args: str, quiet: bool = True) -> str:
    return run(["git", "-C", str(UPSTREAM), *args], quiet=quiet)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest().lower()


def upstream_tag() -> str:
    tag = git("describe", "--tags", "--exact-match", "HEAD").strip()
    if not tag.startswith("v"):
        raise Fail(f"upstream/ HEAD is not at a release tag ({tag})")
    return tag


def ensure_patch() -> None:
    """Applied already (reverse check passes) → nothing to do; else apply 3-way."""
    probe = subprocess.run(["git", "-C", str(UPSTREAM), "apply", "--reverse", "--check", str(PATCH)],
                           capture_output=True)
    if probe.returncode == 0:
        print("hosted.patch is applied")
        return
    res = subprocess.run(["git", "-C", str(UPSTREAM), "apply", "--3way", str(PATCH)],
                         capture_output=True, text=True, encoding="utf-8", errors="replace")
    git("reset", "-q")  # --3way stages it too; keep it in the working tree only (update.py takes it off from there)
    if res.returncode != 0:
        raise Fail("hosted.patch does not apply to this upstream version; resolve the conflict in upstream/, then\n"
                   "  python 工具/update.py --save-patch\n" + res.stdout + res.stderr)
    print("hosted.patch applied")


def download(url: str, target: Path) -> None:
    tmp = target.with_suffix(target.suffix + ".part")
    with urllib.request.urlopen(url, timeout=120) as r, tmp.open("wb") as f:
        shutil.copyfileobj(r, f, 1 << 20)
    tmp.replace(target)


def portable_base(tag: str) -> Path:
    version = tag[1:]
    folder = CACHE / tag
    folder.mkdir(parents=True, exist_ok=True)
    sums = folder / f"Filee-{version}-SHA256SUMS.txt"
    zip_path = folder / f"Filee-{version}-win-Portable.zip"
    base = f"https://github.com/{REPO}/releases/download/{tag}"
    if not sums.exists():
        download(f"{base}/{sums.name}", sums)
    expected = next((line.split()[0].lower() for line in sums.read_text(encoding="utf-8").splitlines()
                     if line.strip().endswith(zip_path.name)), None)
    if not expected:
        raise Fail(f"{sums.name} has no line for {zip_path.name}")
    if not zip_path.exists() or sha256(zip_path) != expected:
        print(f"downloading {zip_path.name} (GitHub is slow on this machine; ~150 MB)")
        download(f"{base}/{zip_path.name}", zip_path)
        if sha256(zip_path) != expected:
            zip_path.unlink()
            raise Fail(f"{zip_path.name}: SHA-256 does not match SHA256SUMS")
    print(f"{zip_path.name} verified")
    extracted = folder / "portable"
    if not (extracted / "filee-cli.exe").exists():
        if extracted.exists():
            shutil.rmtree(extracted)
        with zipfile.ZipFile(zip_path) as z:
            z.extractall(extracted)
        # The zip may hold one top folder.
        inner = [p for p in extracted.iterdir()]
        if len(inner) == 1 and inner[0].is_dir() and (inner[0] / "filee-cli.exe").exists():
            for p in inner[0].iterdir():
                p.rename(extracted / p.name)
            inner[0].rmdir()
    return extracted


def stage(tag: str, portable: Path, exe: Path, app_ver: str) -> None:
    if STAGE.exists():
        shutil.rmtree(STAGE)
    shutil.copytree(portable, STAGE / "app")
    # Patched: Filee.dll (hosted mode, weekly engine update) and Filee.Engines.dll (Microsoft Office engine, 7-Zip
    # MSI), which filee-cli.exe loads from the same folder.
    for name in ("Filee.dll", "Filee.pdb", "Filee.xml", "Filee.Engines.dll", "Filee.Engines.pdb", "Filee.Engines.xml"):
        shutil.copy2(PUBLISH / name, STAGE / "app" / name)
    # The patched DLL must be the one that runs: same size class as the official one (fonts embedded).
    official = (portable / "Filee.dll").stat().st_size
    ours = (STAGE / "app" / "Filee.dll").stat().st_size
    if abs(official - ours) > 1_000_000:
        raise Fail(f"patched Filee.dll is {ours:,} bytes, official {official:,}: fonts or resources missing?")
    shutil.copytree(UPSTREAM / "src" / "Filee.App" / "Assets" / "i18n", STAGE / "i18n")
    shutil.copy2(UPSTREAM / "src" / "Filee.App" / "Assets" / "Icons" / "filee.ico", STAGE / "filee.ico")
    licenses = STAGE / "licenses"
    licenses.mkdir()
    shutil.copy2(UPSTREAM / "LICENSE", licenses / "Filee-LICENSE.txt")
    shutil.copy2(UPSTREAM / "THIRD-PARTY-NOTICES.md", licenses / "Filee-THIRD-PARTY-NOTICES.md")
    shutil.copy2(ROOT / "LICENSE", licenses / "FileeRust-LICENSE.txt")
    shutil.copy2(PATCH, licenses / "hosted.patch")
    shutil.copy2(exe, STAGE / "filee-rust.exe")
    # Self-update (core::self_update): this build's version, and the public key releases must be signed with.
    (STAGE / "version.txt").write_text(app_ver + "\n", encoding="utf-8")
    if PUBLIC_KEY.exists():
        shutil.copy2(PUBLIC_KEY, STAGE / "update.pub")
    else:
        print("note: no packaging/update.pub (uv run 工具/publish.py new-key): this build will not update itself")
    (STAGE / "README.txt").write_text(
        f"Filee Rust for Filee {tag}\r\n\r\n"
        "filee-rust.exe runs in the tray: Ctrl+drag files in Explorer for the donut, Ctrl+Alt+Space for the\r\n"
        "selected files, right-click > Convert with Filee. Settings and tools open upstream Filee on demand.\r\n"
        "Conversions run in app\\filee-cli.exe. Data: %APPDATA%\\Filee (shared with upstream Filee).\r\n"
        "Office documents go to Microsoft Office first when it converts on this PC, else to LibreOffice.\r\n"
        "Filee Rust updates itself from its GitHub releases (signed; tray > Check for updates; the tray menu shows\r\n"
        "this machine's id). Off: {\"autoUpdate\": false} in %LOCALAPPDATA%\\FileeLite\\lite.json.\r\n"
        "The downloaded engines and 7-Zip are updated weekly in the background (logs: %LOCALAPPDATA%\\FileeLite).\r\n\r\n"
        "Filee is MIT licensed by Filee contributors; app\\Filee.dll and app\\Filee.Engines.dll are built from its source with\r\n"
        "licenses\\hosted.patch applied. Third-party notices: licenses\\.\r\n",
        encoding="utf-8")


def smoke(tag: str) -> None:
    cli = STAGE / "app" / "filee-cli.exe"
    version = run([str(cli), "version"], quiet=True).strip()
    if tag[1:] not in version:
        raise Fail(f"staged filee-cli says '{version}', expected {tag}")
    print(f"filee-cli: {version}")
    report = run([str(STAGE / "filee-rust.exe"), "--selftest"], quiet=True)
    print(report.strip())
    if not report.strip().endswith("PASS"):
        raise Fail("filee-rust --selftest failed")
    # One real conversion with the staged CLI, on a copy of the data (the user's own data is never written).
    work = BUILD / "smoke"
    if work.exists():
        shutil.rmtree(work)
    (work / "data").mkdir(parents=True)
    src = Path(os.environ.get("APPDATA", "")) / "Filee"
    for name in ("settings.json", "presets.json", "profiles.json"):
        if (src / name).exists():
            shutil.copy2(src / name, work / "data" / name)
    sample = work / "sample.svg"
    sample.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">'
                      '<rect width="200" height="100" fill="#7F77DD"/></svg>', encoding="utf-8")
    env = dict(os.environ, FILEE_DATA_DIR=str(work / "data"))
    out = run([str(cli), "convert", str(sample), "--to", "png", "-o", str(work / "out"), "--json"], env=env, quiet=True)
    if not (work / "out" / "sample.png").exists():
        raise Fail("smoke conversion made no PNG:\n" + out[-800:])
    print("smoke conversion: ok (svg → png)")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--lite-rev", type=int, default=LITE_REV)
    ap.add_argument("--skip-tests", action="store_true", help="skip cargo test (not for releases)")
    args = ap.parse_args()
    try:
        step("1 patch")
        tag = upstream_tag()
        ensure_patch()
        version = tag[1:]
        app_ver = f"{version}-rust.{args.lite_rev}"
        file_ver = f"{version}.{args.lite_rev}"

        step("2 fonts")
        run([PY, "-I", "-X", "utf8", str(ROOT / "工具" / "fetch_upstream_fonts.py")])

        step("3 dotnet publish")
        env = dict(os.environ, DOTNET_CLI_TELEMETRY_OPTOUT="1", DOTNET_NOLOGO="1")
        run([str(DOTNET), "publish", str(UPSTREAM / "src" / "Filee.App" / "Filee.App.csproj"), "-c", "Release",
             "-r", "win-x64", "--self-contained", "-p:PublishReadyToRun=true", "-o", str(PUBLISH),
             "-v:quiet", "-nologo"], env=env, quiet=True)
        print(f"Filee.dll {(PUBLISH / 'Filee.dll').stat().st_size:,} bytes")

        step("4 official portable")
        portable = portable_base(tag)

        # The shell embeds this version's format table (core/formats_default.txt): regenerate it from the CLI.
        table = SHELL / "src" / "core" / "formats_default.txt"
        fresh = run([str(portable / "filee-cli.exe"), "formats"], quiet=True).replace("\r\n", "\n")
        if fresh.strip() and (not table.exists() or table.read_text(encoding="utf-8").replace("\r\n", "\n") != fresh):
            table.write_text(fresh, encoding="utf-8", newline="\n")
            print("formats_default.txt regenerated for", tag)

        step("6 cargo")  # before staging: the stage needs the exe
        if not args.skip_tests:
            run(["cargo", "test", "--release", "--quiet"], cwd=SHELL, quiet=True)
            print("cargo test: ok")
        run(["cargo", "build", "--release", "--quiet"], cwd=SHELL, quiet=True)
        exe = SHELL / "target" / "release" / "filee-rust.exe"

        step("5 stage")
        stage(tag, portable, exe, app_ver)
        print(f"staged {sum(1 for _ in STAGE.rglob('*') if _.is_file())} files")

        step("7 checks")
        smoke(tag)

        step("8 installer")
        iscc = next((p for p in ISCC_CANDIDATES if p.exists()), None)
        if not iscc:
            raise Fail("Inno Setup 6 (ISCC.exe) not found")
        DIST.mkdir(exist_ok=True)
        run([str(iscc), "/Q", f"/DAppVer={app_ver}", f"/DFileVer={file_ver}", f"/DStage={STAGE}",
             f"/DOutDir={DIST}", str(ROOT / "packaging" / "FileeRust.iss")], quiet=True)
        setup = DIST / f"FileeRust-{app_ver}-Setup.exe"
        if not setup.exists():
            raise Fail(f"{setup.name} was not made")
        manifest = {"upstream": tag, "version": app_ver, "setup": setup.name,
                    "sha256": sha256(setup), "bytes": setup.stat().st_size}
        (DIST / f"FileeRust-{app_ver}.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
        print(f"PASS: {setup} ({setup.stat().st_size / 1e6:.1f} MB)")
        return 0
    except Fail as e:
        print(f"FAIL: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
