# installer — Filee Rust 的安裝／解除安裝程式

這是上游 Filee 自己的安裝程式 `src/Filee.Setup`（Avalonia，MIT，Copyright (c) Filee contributors，授權全文見
[LICENSE-Filee.txt](LICENSE-Filee.txt)）的 **fork**，取代原本的 Inno Setup 腳本。Fork 而不是補丁：改它不會讓 `patches/hosted.patch` 變大。

外觀仍是上游的：640×420 卡片、DonutLogo、ShimmerBar、CheckBurst、SoftBackdrop、選項面板；`Styles/Soft.axaml`、`SoftPalette`、`AppLanguages`、
圖示和 17 語言翻譯都用相對路徑連到 `../upstream/src/Filee.App`（所以要先有 `upstream/`：`python 工具/update.py --init`）。
我們自己的字串在 `i18n/en.json`、`i18n/zh-TW.json`，同名的 key 蓋過上游（其他語言沒有我們的檔時，我們有的 key 用英文，其餘用上游的翻譯）。

## 跟上游的差異

| 項目 | 上游 Filee.Setup | 這個 fork |
|---|---|---|
| 產品 | `Filee.exe`、`Filee` | `filee-rust.exe`、`Filee Rust`；`app\Filee.exe`（hosted）只用來下載引擎 |
| 身分 | Filee 的 AppId `{C8686E06-…}_is1` | Inno 版 Filee Rust 的 AppId `{6B0F3C5E-2D1A-4E57-9E2B-F11EE0717E01}_is1`；原版 Filee 的 key 只讀（找它的資料夾、請它 `--quit`） |
| 預設範圍 | 所有使用者（Program Files） | 目前使用者（`%LOCALAPPDATA%\Programs\Filee Rust`）；選項面板或 `/ALLUSERS` 才裝所有使用者 |
| 選項 | 右鍵選單（含 Win11 頂層）、開機啟動、PATH 的 cli、桌面捷徑 | 開機啟動（`HKCU Run\FileeRust`）、7-Zip、每週引擎更新、六個引擎下載；右鍵選單／PATH／桌面捷徑拿掉 |
| 完成頁 | 「啟動 Filee」 | 只有「完成」；互動安裝結束一律啟動 `filee-rust.exe --settings`（未提權） |
| 靜默 | `/VERYSILENT` 也啟動 Filee | 靜默什麼都不啟動；`/SELFUPDATE=1` 例外：最後一定 `filee-rust.exe --background` |
| 更新說明投影片 | 有 | 拿掉（那是上游的更新說明） |
| `/PORTABLE` | 有 | 拿掉 |
| 解除安裝 | `Filee.exe --uninstall-cleanup`、可選刪設定 | `filee-rust.exe --quit`；刪程式檔、7-Zip、`engine-update.disabled`、Run 值、`HKCU\Software\Classes\*\shell\FileeLite`、捷徑、解除安裝項目；資料一律保留 |
| 打包器 | `build/tools/make-setup.cs`（讀 `EngineDownloads`） | `tools/make-setup.cs`（引擎固定六個，不需 `Filee.Engines`） |

## 命令列

見 [SetupOptions.cs](SetupOptions.cs) 開頭。Inno 的開關照收（`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /DIR= /TASKS= /MERGETASKS= /LOG= /NOICONS /CURRENTUSER /ALLUSERS …`），
工作名稱：`startup`、`sevenzip`、`engineupdate`、`engines`、`engines\libreoffice|tesseract|pandoc|ghostscript|ffmpeg|calibre`。
已安裝的 Filee Rust 自更新時跑的是 `Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /SELFUPDATE=1 /LOG=<檔>`（舊外殼，改不了）。

## 建置與測試

```powershell
python 工具\build_release.py                # 整個產品，步驟 8 發佈這個專案並打包
python 工具\build_release.py --setup-only   # 只做步驟 8（沿用 build\stage）
dotnet test installer\tests                 # 參數解析、選項、安裝／解除安裝的檔案與登錄結果（記憶體登錄檔，不啟動任何程式）
```
