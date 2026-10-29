# 研究筆記：Filee 要不要、怎麼改寫成 Rust（2026-10-29）

## 上游現況（實測／查證）

| 項目 | 數字 | 來源 |
|---|---|---|
| 授權 | MIT（Filee contributors） | GitHub API `license.spdx_id` |
| 星數 | 16,155 | GitHub API |
| 語言 | C# 9.4 MB（測試含在內），PowerShell 66 KB，Python 33 KB | GitHub languages |
| 正式程式碼 | 約 5.6 MB C#（≈13 萬行）：Engines 3.3 MB、App 1.3 MB、Core 0.5 MB | git tree 加總 |
| 發版頻率 | 30 天 30 版（v1.0.0 2026-09-29 → v2.2.0 2026-10-28） | releases |
| 常駐記憶體 | Working Set 299 MB、Private 213 MB、95 執行緒（閒置） | `Get-Process Filee` |
| CLI 單次轉檔 | PNG→WebP 0.53 s、峰值 89 MB，結束即釋放 | 實測 |
| hosted 上游（補丁版）| 一次 `--run=to-png`：峰值 270 MB，8 秒後自己結束 | 實測 |

記憶體大戶：.NET runtime＋Avalonia（Skia、ANGLE、合成）＋所有引擎 DLL（Magick.Native 24 MB、pdfium、PdfPig…）
在啟動時全載入；它其實只是在等一個 Ctrl 拖曳。

## 為什麼不整套照搬

- 20 類引擎大量依賴 .NET 專屬函式庫（PdfSharp、PdfPig、ACadSharp、MsgReader、Magick.NET…），Rust 沒有一對一替代。
- 一天一版：整套翻譯的版本永遠落後；每版都要人工把 C# 改動翻成 Rust。
- 互動工具（裁切、PDF 頁面編輯、影片剪輯時間軸、試算表列印工作室）背後是引擎邏輯（改 PDF crop box、FFmpeg filter
  graph、LibreOffice 列印包裝），重寫等於重寫引擎。

## 結論：三層

1. **Rust 常駐外殼**（`shell/`）：只做「等待與選擇」——hook、donut、toast、系統匣、watch、右鍵。預估閒置 10–30 MB。
2. **上游 CLI**（`filee-cli.exe`）：每次轉檔起一個、轉完結束。轉換能力 100% 是上游的，上游改版直接換掉就跟上。
3. **上游 GUI，hosted**（補丁版 `Filee.exe --hosted`）：設定頁、互動工具、合併順序、存檔對話框。要用才開，關掉就結束。

補丁只有 4 個檔、+117/−9 行（`patches/hosted.patch`），每版用 `git apply --3way` 套。

## 封裝來源

- 官方 `Filee-<ver>-win-Portable.zip`（SHA256SUMS 驗證）當底：所有相依、內建引擎（7-Zip、rhwp）、右鍵 DLL 都是官方原檔。
- 只覆蓋補丁版 `Filee.dll`（同 tag 原始碼＋補丁，ReadyToRun）。
- 上游的 Noto 字型不在 git：`build/fetch-fonts.ps1` 下載。我們不執行它，`工具/fetch_upstream_fonts.py`
  從該版腳本**解析**網址與 SHA-256 自己下載驗證。少了字型 Filee.dll 會小 39 MB（12.9 vs 52.0 MB），
  有了之後 51,978,240 vs 官方 51,980,416（差的是補丁）。

## 引擎

- 實測環境：LibreOffice 26.8.1.1、Tesseract 5.4.0、Pandoc 3.13（winget）、Ghostscript 10.08.0（Artifex 官方，簽章驗證），
  FFmpeg 9.0.2（winget，原本就有）。Filee 從 PATH 與 Program Files 找得到（Tesseract 加進使用者 PATH）。
- 別台：安裝檔的「線上安裝轉換引擎」勾選項，交給**上游自己的下載器**（`Filee.exe --hosted --background
  --install-engines=libreoffice,tesseract,…`）：版本與 SHA-256 由上游鎖定、裝到 `%LOCALAPPDATA%\Filee\engines`、不需要系統管理員。
  原本規劃 winget＋自寫下載腳本，改成這樣是因為：不跳 UAC、版本跟上游同步、不用在安裝檔裡放「下載並執行 exe」的腳本
  （防毒軟體可能把這種腳本判成 Downloader）。

## 300 DPI

使用者要求所有轉圖片預設 300 DPI。上游 preset 的 `pdf.renderDpi` 預設 150（PDF／文件頁面轉圖片的實際解析度），
`image.dpi` 預設 null（保留來源標記）。`工具/filee_300dpi.py` 把 renderDpi < 300 拉到 300、點陣圖輸出 image.dpi=300；
實測 A4 PDF → PNG／JPG 都是 2480×3507、300 DPI。外殼的 `core::integration` 每次啟動也會套（上游新版加的預設會帶回 150）。

## 追上游新版的做法（閉環）

`python 工具/update.py`（細節在檔頭說明）：

1. 查 GitHub `releases/latest`，一天最多一次（狀態存在 `cache/update-state.json`，不輪詢）。
2. `git fetch` 新 tag 到 `upstream/`（sparse，不 checkout 建置腳本）。
3. 寫 `docs/upstream-diffs/<舊>_to_<新>.md`：每個改動的檔分成三類——
   **外殼要跟**（`SHELL_MAP` 列的上游檔：觸發、donut、toast、匣、主題、資料 schema、CLI 輸出 → 對應 Rust 模組）、
   **補丁碰到**（`hosted.patch` 的 4 個檔，衝突會在這裡）、**自動跟上**（其餘：引擎、工具視窗、翻譯，重編就有）；附 CHANGELOG 段落。
4. 退補丁 → checkout 新 tag → `git apply --3way` 補丁；衝突就停、告訴人改哪裡。
5. `工具/build_release.py`：字型 → `dotnet publish` → 官方 Portable（SHA256SUMS）→ `cargo test`／`build` → 組裝 → `--selftest`
   ＋真的轉一次 → Inno Setup → `dist/FileeLite-<ver>-lite.1-Setup.exe`。
6. 結束碼 0 = 直接可用；2 = 安裝檔建好了，但報告第 1 節有外殼要跟的改動，人看過、移植、`--rebuild`。

## 風險

- **防毒相容（實測過 Bitdefender）**：自編未簽章 exe 掛全域 hook＋開機自啟，2026-10-26 被擋過。外殼不自己寫 Run 機碼（安裝檔的勾選項寫），
  使用者要在 Bitdefender 對 `filee-lite.exe` 加信任。
- **SmartScreen（別台）**：安裝檔未簽章，要按「其他資訊 → 仍要執行」。
- **共用資料檔**：上游 GUI 存檔時整檔覆寫、不重讀。hosted 開著時外殼的寫入（history、paused、300 DPI）排隊到它結束。
- **原版 Filee 同時在跑**：兩個 hook、兩個 donut。安裝檔會請它結束並建議解除安裝（設定共用，不會遺失）。
