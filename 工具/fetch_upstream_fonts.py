"""下載上游 Filee 內嵌的字型到 upstream/src/Filee.App/Assets/Fonts/。

上游用 build/fetch-fonts.ps1 在建置前下載字型（git 不收錄，.gitignore 排除）。少了它們，自己編的 Filee.dll
少 39 MB、介面改用系統字型。這支**不執行**上游腳本：從 git 讀出該版的腳本文字，只解析
「$變數 = '網址'」與「'檔名' = @("網址", '雜湊')」兩種行，自己下載並比對 SHA-256。
上游換字型或版本時，重跑就會跟著換（閉環的一步）。

用法：python fetch_upstream_fonts.py [--ref v2.2.0]
"""
import argparse
import hashlib
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UPSTREAM = ROOT / "upstream"
CACHE = ROOT / "cache" / "fonts"
DEST = UPSTREAM / "src" / "Filee.App" / "Assets" / "Fonts"

VAR_RE = re.compile(r"""^\s*\$(\w+)\s*=\s*['"]([^'"]+)['"]\s*$""")
ENTRY_RE = re.compile(r"""^\s*'([^']+\.(?:ttf|otf))'\s*=\s*@\(\s*"([^"]+)"\s*,\s*'([0-9A-Fa-f]{64})'\s*\)""")


def script_text(ref: str) -> str:
    out = subprocess.run(["git", "-C", str(UPSTREAM), "show", f"{ref}:build/fetch-fonts.ps1"],
                         capture_output=True, check=True)
    return out.stdout.decode("utf-8-sig")


def parse(text: str) -> list[tuple[str, str, str]]:
    """回傳 (檔名, 網址, SHA-256)；網址裡的 $變數 用前面定義的值展開。"""
    variables: dict[str, str] = {}
    fonts = []

    def expand(s: str) -> str:
        return re.sub(r"\$(\w+)", lambda m: variables.get(m.group(1), m.group(0)), s)

    for line in text.splitlines():
        if m := ENTRY_RE.match(line):
            fonts.append((m.group(1), expand(m.group(2)), m.group(3).upper()))
        elif m := VAR_RE.match(line):
            variables[m.group(1)] = expand(m.group(2))
    return fonts


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest().upper()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ref", default="HEAD", help="上游的 tag 或 commit（預設目前 checkout）")
    args = ap.parse_args()

    fonts = parse(script_text(args.ref))
    if not fonts:
        print("錯誤：fetch-fonts.ps1 解析不到任何字型（上游改了格式？）")
        return 1
    if any("$" in url for _, url, _ in fonts):
        print("錯誤：有網址的變數沒展開：", [u for _, u, _ in fonts if "$" in u])
        return 1
    if any(not url.startswith("https://") for _, url, _ in fonts):
        print("錯誤：只接受 https 網址")
        return 1

    CACHE.mkdir(parents=True, exist_ok=True)
    DEST.mkdir(parents=True, exist_ok=True)
    for name, url, expected in fonts:
        cached = CACHE / f"{expected[:16]}-{name}"
        if not cached.exists() or sha256(cached) != expected:
            tmp = cached.with_suffix(cached.suffix + ".part")
            with urllib.request.urlopen(url, timeout=60) as r, tmp.open("wb") as f:
                shutil.copyfileobj(r, f)
            actual = sha256(tmp)
            if actual != expected:
                tmp.unlink()
                print(f"錯誤：{name} 雜湊不符（{actual} ≠ {expected}）")
                return 1
            tmp.replace(cached)
            print(f"下載 {name}")
        shutil.copy2(cached, DEST / name)
    stale = [p.name for p in DEST.iterdir() if p.suffix in (".ttf", ".otf") and p.name not in {n for n, _, _ in fonts}]
    for name in stale:
        (DEST / name).unlink()
    print(f"過：{len(fonts)} 個字型就位於 {DEST}" + (f"（移除舊的 {stale}）" if stale else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
