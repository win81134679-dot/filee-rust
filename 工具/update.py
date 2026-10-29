"""The closed loop: upstream Filee released a new version → see what changed → re-patch, rebuild, repackage.

  python 工具/update.py            check (at most once a day), and if there is a newer release: update + build
  python 工具/update.py --check    only say whether a newer release exists (exit 10 if so)
  python 工具/update.py --force    check now even if checked today
  python 工具/update.py --to v2.3.0  move to that tag (also older ones), then build
  python 工具/update.py --rebuild  build the current tag again (after a shell/ change)
  python 工具/update.py --save-patch  write upstream/'s changes (new files too) into patches/hosted.patch
  python 工具/update.py --init     fresh clone: fetch upstream at patches/UPSTREAM_TAG into upstream/, patch on
  python 工具/update.py --publish [--machines A,B]  sign the newest dist/ installer, upload it as a GitHub
                                   pre-release; installed copies update themselves (工具/publish.py, asks the password)
  python 工具/update.py --open v2.2.0-rust.5        open a release made for some machines to every machine

What it does for a new tag:
  1. git fetch the tag into upstream/ (sparse: src/, docs/, root files; no build scripts are checked out)
  2. write docs/upstream-diffs/<old>_to_<new>.md: CHANGELOG lines, and every changed file sorted into
     "shell must follow" (what shell/ ports: triggers, donut, toast, tray, theme, data schema, CLI output …),
     "patched files" (patches/hosted.patch touches them: conflicts show up here) and "follows by itself"
     (engines, tools, translations: they come with the rebuilt upstream, nothing to port)
  3. take hosted.patch off, check out the new tag, put the patch back on (3-way; a conflict stops here)
  4. 工具/build_release.py → dist/FileeRust-<new>-rust.<LITE_REV>-Setup.exe
Exit codes: 0 done (nothing for a person to look at), 2 built but the report lists shell changes to port,
1 failed (message says where), 10 (--check) a newer release exists.
State: cache/update-state.json (last check date, last built tag) so the GitHub API is asked once a day at most.
"""
import argparse
import datetime as dt
import json
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UPSTREAM = ROOT / "upstream"
PATCH = ROOT / "patches" / "hosted.patch"
TAG_FILE = ROOT / "patches" / "UPSTREAM_TAG"  # the upstream tag hosted.patch is written against
UPSTREAM_URL = "https://github.com/KnifeLemon/Filee.git"
# Sparse checkout: sources and docs; upstream's build scripts are never checked out (they download and run things).
SPARSE = ["/src/", "/docs/", "/CHANGELOG.md", "/LICENSE", "/THIRD-PARTY-NOTICES.md", "/README.md", "/README.zh-TW.md",
          "/Directory.Packages.props", "/Directory.Build.props", "/global.json", "/Filee.slnx", "/.editorconfig",
          "!*.ps1", "!*.sh", "!*.cmd", "!*.bat"]
STATE = ROOT / "cache" / "update-state.json"
DIFFS = ROOT / "docs" / "upstream-diffs"
REPO = "KnifeLemon/Filee"

# Upstream paths whose behaviour shell/ re-implements → Rust module that has to follow.
SHELL_MAP = [
    ("src/Filee.App/Services/Triggers/", "shell/src/input"),
    ("src/Filee.Platform.Windows/", "shell/src/input (Explorer selection, scope), core::integration"),
    ("src/Filee.App/Controls/Donut", "shell/src/donut"),
    ("src/Filee.App/Views/RadialWindow", "shell/src/donut"),
    ("src/Filee.App/ViewModels/RadialViewModel", "shell/src/donut, core::profiles"),
    ("src/Filee.App/Services/RadialController", "shell/src/donut, main.rs"),
    ("src/Filee.App/Services/DraggedFiles", "shell/src/donut (drop formats)"),
    ("src/Filee.App/Services/BrowserDrags", "shell/src/donut (drop formats)"),
    ("src/Filee.App/Services/PresetAvailability", "core::profiles"),
    ("src/Filee.App/Services/Motion", "shell/src/donut, ui::toast"),
    ("src/Filee.App/Views/ToastWindow", "ui::toast"),
    ("src/Filee.App/ViewModels/JobViewModel", "ui::toast"),
    ("src/Filee.App/Services/TrayService", "ui::tray"),
    ("src/Filee.App/Services/ThemeService", "ui::theme"),
    ("src/Filee.App/Services/SoftPalette", "ui::theme"),
    ("src/Filee.App/Services/MaterialYouPalette", "ui::theme"),
    ("src/Filee.App/Styles/", "ui::theme (colours)"),
    ("src/Filee.App/Services/LocalizationService", "ui::i18n"),
    ("src/Filee.App/Services/AppLanguages", "ui::i18n"),
    ("src/Filee.App/Services/ConversionService", "core::jobs, core::history"),
    ("src/Filee.App/Services/WatchFolderService", "core::watch"),
    ("src/Filee.App/Services/SystemIntegrationService", "core::integration"),
    ("src/Filee.Core/Settings/", "core::store (schema), input (triggers)"),
    ("src/Filee.Core/Profiles/", "core::profiles"),
    ("src/Filee.Core/Presets/Preset.cs", "core::profiles, core::integration (300 DPI)"),
    ("src/Filee.Core/Watching/", "core::watch"),
    ("src/Filee.Core/History", "core::history"),
    ("src/Filee.Cli/", "core::jobs (CLI output format)"),
]


def git(*args: str, check: bool = True) -> str:
    out = subprocess.run(["git", "-C", str(UPSTREAM), *args], capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    if check and out.returncode != 0:
        raise SystemExit(f"FAIL: git {' '.join(args)}\n{out.stdout}{out.stderr}")
    return out.stdout


def load_state() -> dict:
    try:
        return json.loads(STATE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    STATE.write_text(json.dumps(state, indent=2), encoding="utf-8")


def version_key(tag: str) -> tuple:
    return tuple(int(x) for x in re.findall(r"\d+", tag)[:3])


def latest_release() -> str:
    req = urllib.request.Request(f"https://api.github.com/repos/{REPO}/releases/latest",
                                 headers={"Accept": "application/vnd.github+json", "User-Agent": "filee-rust-update"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)["tag_name"]


def current_tag() -> str:
    return git("describe", "--tags", "--exact-match", "HEAD").strip()


def classify(old: str, new: str) -> tuple[list, list, list]:
    patched = set(re.findall(r"^\+\+\+ b/(\S+)", PATCH.read_text(encoding="utf-8"), re.M))
    shell, patch_hits, auto = [], [], []
    for line in git("diff", "--numstat", old, new, "--", "src").splitlines():
        added, removed, path = line.split("\t", 2)
        item = (path, added, removed)
        if path in patched:
            patch_hits.append(item)
        target = next((module for prefix, module in SHELL_MAP if path.startswith(prefix)), None)
        if target:
            shell.append((*item, target))
        elif path not in patched:
            auto.append(item)
    return shell, patch_hits, auto


def changelog_between(old: str, new: str) -> str:
    text = git("show", f"{new}:CHANGELOG.md", check=False)
    lo, hi = version_key(old), version_key(new)
    keep, out = False, []
    for line in text.splitlines():
        m = re.match(r"^##\s*\[?v?(\d+\.\d+\.\d+)", line)
        if m:
            keep = lo < version_key(m.group(1)) <= hi
        if keep:
            out.append(line)
    return "\n".join(out).strip() or "(CHANGELOG has no entries between these versions)"


def write_report(old: str, new: str) -> tuple[Path, int]:
    shell, patch_hits, auto = classify(old, new)
    DIFFS.mkdir(parents=True, exist_ok=True)
    report = DIFFS / f"{old}_to_{new}.md"
    lines = [f"# 上游 Filee {old} → {new}", "",
             f"產生：{dt.datetime.now():%Y-%m-%d %H:%M}（工具/update.py）", "",
             "## 1. 外殼要跟的改動（shell/ 移植了這些行為）", ""]
    if shell:
        lines += ["| 檔案 | +/- | 對應的 Rust 模組 |", "|---|---|---|"]
        lines += [f"| `{p}` | +{a}/-{r} | {m} |" for p, a, r, m in shell]
        lines += ["", "看法：`git -C upstream diff " + old + " " + new + " -- <檔案>`，把行為差異搬進對應模組，"
                  "`cargo test`，再 `python 工具/update.py --rebuild`。"]
    else:
        lines.append("沒有。")
    lines += ["", "## 2. 補丁碰到的檔（patches/hosted.patch）", ""]
    lines += [f"- `{p}` +{a}/-{r}" for p, a, r in patch_hits] or ["沒有：補丁應可直接套用。"]
    lines += ["", "## 3. 自動跟上（引擎、工具視窗、翻譯…：重編上游就有）", "",
              f"{len(auto)} 個檔。", ""]
    lines += [f"- `{p}` +{a}/-{r}" for p, a, r in auto[:200]]
    if len(auto) > 200:
        lines.append(f"- …另 {len(auto) - 200} 個")
    lines += ["", "## 4. CHANGELOG", "", changelog_between(old, new), ""]
    report.write_text("\n".join(lines), encoding="utf-8")
    return report, len(shell)


def move_to(new: str) -> None:
    git("fetch", "--depth", "1", "--filter=blob:none", "origin", "tag", new, "--no-tags")
    # Our patch lives in the working tree: take it off, switch, put it back.
    if subprocess.run(["git", "-C", str(UPSTREAM), "apply", "--reverse", "--check", str(PATCH)],
                      capture_output=True).returncode == 0:
        git("apply", "--reverse", str(PATCH))
    status = git("status", "--porcelain", "--untracked-files=no").strip()
    if status:
        raise SystemExit("FAIL: upstream/ has changes that are not hosted.patch; save them into the patch first:\n"
                         + status)
    git("-c", "advice.detachedHead=false", "checkout", new)
    res = subprocess.run(["git", "-C", str(UPSTREAM), "apply", "--3way", str(PATCH)],
                         capture_output=True, text=True, encoding="utf-8", errors="replace")
    # --3way also stages the change; keep it in the working tree only, so taking it off again leaves a clean index.
    git("reset", "-q")
    if res.returncode != 0:
        raise SystemExit("FAIL: hosted.patch conflicts with " + new + ". Fix the files git marked in upstream/, then\n"
                         "  python 工具/update.py --save-patch\n  python 工具/update.py --rebuild\n"
                         + res.stdout + res.stderr)
    # Rebase the patch onto this version: its context now matches, so the next 3-way apply starts clean.
    refreshed = patch_text()
    if refreshed.strip():
        PATCH.write_text(refreshed, encoding="utf-8", newline="\n")
    TAG_FILE.write_text(new + "\n", encoding="utf-8", newline="\n")


def init_upstream() -> None:
    """A fresh clone of this repository has no upstream/: fetch the tag the patch is written against, patch on."""
    if (UPSTREAM / ".git").exists():
        raise SystemExit(f"upstream/ is already there (at {current_tag()}); nothing to do")
    tag = TAG_FILE.read_text(encoding="utf-8").strip()
    subprocess.run(["git", "init", "-q", str(UPSTREAM)], check=True)
    git("remote", "add", "origin", UPSTREAM_URL)
    git("config", "remote.origin.promisor", "true")
    git("config", "remote.origin.partialclonefilter", "blob:none")
    git("sparse-checkout", "set", "--no-cone", *SPARSE)
    print(f"fetching {tag} from {UPSTREAM_URL} (sources only, about 10 MB) …", flush=True)
    git("fetch", "--depth", "1", "--filter=blob:none", "origin", "tag", tag, "--no-tags")
    git("-c", "advice.detachedHead=false", "checkout", tag)
    res = subprocess.run(["git", "-C", str(UPSTREAM), "apply", "--3way", str(PATCH)],
                         capture_output=True, text=True, encoding="utf-8", errors="replace")
    git("reset", "-q")
    if res.returncode != 0:
        raise SystemExit("FAIL: hosted.patch does not apply to " + tag + "\n" + res.stdout + res.stderr)
    print(f"upstream/ at {tag} with hosted.patch; next: python 工具/build_release.py")


def patch_text() -> str:
    """upstream/'s working-tree changes as a patch, new files included (marked intent-to-add for the diff only)."""
    new_files = [f for f in git("ls-files", "--others", "--exclude-standard", "--", "src").splitlines() if f.strip()]
    if new_files:
        git("add", "--intent-to-add", "--", *new_files)
    try:
        return git("diff")
    finally:
        git("reset", "-q")


def save_patch() -> None:
    text = patch_text()
    if not text.strip():
        raise SystemExit("FAIL: upstream/ has no changes; hosted.patch left as it is")
    PATCH.write_text(text, encoding="utf-8", newline="\n")
    TAG_FILE.write_text(current_tag() + "\n", encoding="utf-8", newline="\n")
    print(f"hosted.patch: {sum(1 for l in text.splitlines() if l.startswith('diff --git'))} file(s)")


def build() -> int:
    return subprocess.run([sys.executable, "-I", "-X", "utf8", str(ROOT / "工具" / "build_release.py")]).returncode


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--rebuild", action="store_true")
    ap.add_argument("--to")
    ap.add_argument("--no-build", action="store_true", help="move + report only (tests the loop)")
    ap.add_argument("--save-patch", action="store_true", help="upstream/ changes → patches/hosted.patch")
    ap.add_argument("--init", action="store_true", help="fresh clone: fetch upstream at patches/UPSTREAM_TAG")
    ap.add_argument("--publish", action="store_true", help="newest dist/ installer → signed GitHub pre-release")
    ap.add_argument("--machines", help="with --publish: only these machine ids / machines.json names")
    ap.add_argument("--open", metavar="TAG", help="a release made for some machines → every machine")
    args = ap.parse_args()
    if args.publish or args.open:
        tool = ["uv", "run", "--quiet", str(ROOT / "工具" / "publish.py")]
        if args.open:
            return subprocess.run(tool + ["open", args.open]).returncode
        return subprocess.run(tool + ["publish"] + (["--machines", args.machines] if args.machines else [])).returncode
    if args.init:
        init_upstream()
        return 0
    if args.save_patch:
        save_patch()
        return 0

    state = load_state()
    current = current_tag()
    if args.rebuild:
        code = build()
        if code == 0:
            state["built"] = current
            save_state(state)
        return code

    today = dt.date.today().isoformat()
    if args.to:
        target = args.to
    else:
        if state.get("checked") == today and not args.force:
            target = state.get("latest", current)
            print(f"今天已查過：上游最新 {target}（--force 重查）")
        else:
            target = latest_release()
            state.update(checked=today, latest=target)
            save_state(state)
        if args.check:
            newer = version_key(target) > version_key(current)
            print(f"目前 {current}，上游最新 {target}：" + ("有新版" if newer else "已是最新"))
            return 10 if newer else 0
        if version_key(target) <= version_key(current):
            if state.get("built") == current:
                print(f"已是最新（{current}），也已建置過。")
                return 0
            print(f"已是最新（{current}），但還沒建置過：建置")
            code = build()
            if code == 0:
                state["built"] = current
                save_state(state)
            return code

    print(f"{current} → {target}")
    move_to(target)
    report, shell_changes = write_report(current, target)
    print(f"差異報告：{report}")
    if args.no_build:
        print(f"--no-build：upstream/ 已在 {target}（補丁已套），未建置")
        return 2 if shell_changes else 0
    code = build()
    if code != 0:
        return code
    state["built"] = target
    save_state(state)
    if shell_changes:
        print(f"注意：{shell_changes} 個外殼移植過的檔有改動，見報告第 1 節（安裝檔已建好，行為以舊版移植為準）。")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
