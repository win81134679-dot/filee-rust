"""Install the newest dist/FileeRust-*-Setup.exe silently into build/test-install, check it, uninstall it.

  python 工具/test_install.py [--keep]

What it does (nothing outside build/test-install, except what any installer does: Inno Setup unpacks its setup
program into %TEMP% and writes an uninstall entry under HKCU, which the uninstall at the end removes again):
  1. Setup.exe /VERYSILENT /CURRENTUSER /DIR=build\\test-install /TASKS="" (no autostart, no engine download)
  2. filee-rust.exe --selftest → PASS
  3. the installed filee-cli converts an SVG → PNG on a copy of the data (FILEE_DATA_DIR)
  4. unins000.exe /VERYSILENT (skipped with --keep)
Run it yourself: some antivirus products watch installers that unpack into %TEMP%.
"""
import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / "build" / "test-install"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave the test install in place")
    args = ap.parse_args()
    setups = sorted((ROOT / "dist").glob("FileeRust-*-Setup.exe"), key=lambda p: p.stat().st_mtime)
    if not setups:
        print("FAIL: no dist/FileeRust-*-Setup.exe; run 工具/build_release.py first")
        return 1
    setup = setups[-1]
    if TARGET.exists():
        shutil.rmtree(TARGET)
    print(f"installing {setup.name} → {TARGET}")
    r = subprocess.run([str(setup), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER",
                        f"/DIR={TARGET}", '/TASKS=""', "/NOICONS", f"/LOG={ROOT / 'build' / 'test-install.log'}"])
    if r.returncode != 0 or not (TARGET / "filee-rust.exe").exists():
        print(f"FAIL: setup exit {r.returncode}; see build/test-install.log")
        return 1

    ok = True
    st = subprocess.run([str(TARGET / "filee-rust.exe"), "--selftest"], capture_output=True, text=True,
                        encoding="utf-8", errors="replace", env={k: v for k, v in os.environ.items() if k != "FILEE_LITE_APP"})
    print(st.stdout.strip())
    ok &= st.stdout.strip().endswith("PASS")

    work = ROOT / "build" / "test-install-data"
    if work.exists():
        shutil.rmtree(work)
    (work / "data").mkdir(parents=True)
    sample = work / "sample.svg"
    sample.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><circle cx="60" cy="40" r="30" '
                      'fill="#7F77DD"/></svg>', encoding="utf-8")
    cv = subprocess.run([str(TARGET / "app" / "filee-cli.exe"), "convert", str(sample), "--to", "png", "-o",
                         str(work / "out")], capture_output=True, text=True, encoding="utf-8", errors="replace",
                        env=dict(os.environ, FILEE_DATA_DIR=str(work / "data")))
    converted = (work / "out" / "sample.png").exists()
    print(f"installed filee-cli: {'ok' if converted else 'FAIL'} {cv.stdout.strip()[-200:]}")
    ok &= converted

    if not args.keep:
        uninstaller = next(TARGET.glob("unins*.exe"), None)
        if uninstaller:
            subprocess.run([str(uninstaller), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            print("uninstalled")
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
