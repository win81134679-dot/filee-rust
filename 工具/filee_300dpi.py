"""把 Filee 的預設（presets.json）改成「轉圖片一律 300 DPI」。

使用者 2026-10-29：「幫我預設不管甚麼轉圖片都要300DPI 不然不清楚」

改兩個欄位：
- pdf.renderDpi：PDF／文件頁面轉成圖片時的實際解析度（上游預設 150）。低於 300 的拉到 300。
- image.dpi：寫進點陣圖檔的 DPI 標記（只改標記，不改像素）。輸出是點陣圖的預設設成 300。

上游改版或新增預設時會帶回 150，所以這支要能重複跑；--check 只檢查不改（有要改的就結束碼 1）。
Filee 執行中可能把記憶體裡的預設寫回去，改之前先 `Filee.exe --quit`。

用法：
  python filee_300dpi.py [--check] [--presets <presets.json 路徑>]
"""
import argparse
import json
import os
import shutil
import sys
from datetime import datetime
from pathlib import Path

TARGET_DPI = 300

# 點陣圖輸出格式；'*' 是同格式的影像操作（旋轉、灰階、縮半…）
RASTER_TARGETS = {
    "png", "jpg", "jpeg", "webp", "tiff", "bmp", "gif", "avif", "heic", "jxl",
    "psd", "tga", "ico", "icns", "*",
}


def default_presets_path() -> Path:
    return Path(os.environ["APPDATA"]) / "Filee" / "presets.json"


def plan_changes(presets: list) -> list:
    """回傳 (預設 id, 欄位, 舊值, 新值) 的清單，不改動輸入。"""
    changes = []
    for p in presets:
        pdf = p.get("pdf")
        if isinstance(pdf, dict):
            old = pdf.get("renderDpi")
            if not isinstance(old, (int, float)) or old < TARGET_DPI:
                changes.append((p.get("id"), "pdf.renderDpi", old, TARGET_DPI))
        image = p.get("image")
        if p.get("targetFormat") in RASTER_TARGETS and isinstance(image, dict):
            old = image.get("dpi")
            if old != TARGET_DPI:
                changes.append((p.get("id"), "image.dpi", old, TARGET_DPI))
    return changes


def apply_changes(presets: list) -> None:
    for p in presets:
        pdf = p.get("pdf")
        if isinstance(pdf, dict):
            old = pdf.get("renderDpi")
            if not isinstance(old, (int, float)) or old < TARGET_DPI:
                pdf["renderDpi"] = TARGET_DPI
        image = p.get("image")
        if p.get("targetFormat") in RASTER_TARGETS and isinstance(image, dict):
            image["dpi"] = TARGET_DPI


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只檢查，不改")
    ap.add_argument("--presets", type=Path, default=None)
    args = ap.parse_args()

    path = args.presets or default_presets_path()
    if not path.exists():
        print(f"找不到 {path}（Filee 還沒跑過一次？）")
        return 2

    raw = path.read_bytes()
    had_bom = raw.startswith(b"\xef\xbb\xbf")
    presets = json.loads(raw.decode("utf-8-sig"))
    changes = plan_changes(presets)

    if not changes:
        print(f"過：{len(presets)} 個預設都已是 {TARGET_DPI} DPI")
        return 0

    for pid, field, old, new in changes:
        print(f"  {pid:<28} {field:<15} {old} -> {new}")
    print(f"共 {len(changes)} 處要改（{len(presets)} 個預設）")
    if args.check:
        return 1

    backup = path.with_name(f"presets.backup-{datetime.now():%Y%m%d-%H%M%S}.json")
    shutil.copy2(path, backup)
    apply_changes(presets)
    text = json.dumps(presets, ensure_ascii=False, indent=2)
    path.write_bytes((b"\xef\xbb\xbf" if had_bom else b"") + text.encode("utf-8"))
    print(f"已改好，備份在 {backup}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
