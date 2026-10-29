# Filee Lite — 按住 Ctrl 拖移轉檔，常駐只吃十幾 MB

[Filee](https://github.com/KnifeLemon/Filee) 是很好用的離線轉檔工具：在檔案總管按住 Ctrl 拖檔案，跳出環形選單，放下就轉好
（圖片、PDF、Office、影音、電子書、CAD… 263 種格式）。缺點是它一直開著就吃 **約 300 MB** 記憶體。

**Filee Lite** 把「一直開著」的那部分用 Rust 重寫：

- 常駐的 `filee-lite.exe` 只負責等你的手勢、畫環形選單、顯示進度、系統匣。
- 真正轉檔時才叫上游的 `filee-cli.exe`，轉完就結束（實測一次 0.5 秒、89 MB，用完歸還）。
- 設定頁、裁切、剪輯、PDF 頁面編輯、列印工作室這些視窗，要用時才臨時開上游的 Filee，關掉就結束。
- 設定、預設、紀錄跟上游**共用同一份**（`%APPDATA%\Filee`），所以從原版換過來什麼都不用重設。

![架構](docs/架構圖/PNG/執行架構圖.png)

## 安裝（任何一台 Windows 10/11 x64）

1. 執行 `dist\FileeLite-<版本>-Setup.exe`。未簽章，Windows 會跳 SmartScreen：按「其他資訊」→「仍要執行」。
2. 預設裝給目前使用者（不需要系統管理員）。選項**全部預設勾選**：
   - 「登入 Windows 時自動啟動」
   - 「7-Zip 解壓縮引擎」：ZIP、7Z、RAR、TAR、ISO 光碟映像。放在安裝檔裡，不用下載。
   - 「每週自動更新轉換引擎」：在背景靜默執行，只在沒有轉檔、沒用電池的時候跑（見下方〈引擎〉）。
   - 「線上安裝轉換引擎」：由 Filee 官方的下載器下載並驗證，包括
     - LibreOffice（開源 Office 套件，沒有可用的 Microsoft Office 時接手）
     - Tesseract（OCR）
     - Pandoc（Markdown 引擎）
     - Ghostscript（EPS、PS）
     - FFmpeg（影音）
     - Calibre（電子書）

   圖片、資料（CSV、JSON、XML…）、2D 製圖（DXF、DWG）、PDF 都是內建的，不用裝。3D 模型轉換也內建，只是不特別列出來。
3. 電腦上裝了原版 Filee 的話，安裝程式會請它先結束；裝好後建議到「設定 → 應用程式」解除安裝原版（兩個同時開會跳兩個選單）。
4. 有防毒軟體（例如 Bitdefender）把 `filee-lite.exe` 擋下或刪掉的話，到防毒軟體裡把安裝資料夾加入信任。
   它會偵測全域滑鼠鍵盤（只聽不攔），沒簽章的程式這樣做容易被誤判。

## 使用

| 動作 | 做法 |
|---|---|
| 轉檔 | 在檔案總管按住 **Ctrl** 拖檔案 → 拖到環形選單的格式上放開 |
| 轉選取的檔 | 選好檔案按 **Ctrl+Alt+Space** → 點格式 |
| 右鍵 | 檔案上按右鍵 → 「用 Filee 轉換」 |
| 翻到背面（更多格式） | 選單開著時按 **Tab** 或滾輪 |
| 設定 | 點系統匣的 Filee 圖示（開上游的設定視窗） |
| 暫停 | 系統匣右鍵 → 暫停 |

轉成圖片一律 **300 DPI**（PDF／文件頁面轉圖片的解析度與檔案標記），這是本專案的預設。

## 引擎

**Office 文件（Word、Excel、PowerPoint）優先交給 Microsoft Office 轉**，轉不了才改用 LibreOffice：

- 「偵測到」的判準是在這台電腦上真的試轉成功。Word、Excel、PowerPoint 各自先轉一個小檔成 PDF，成功的才算。
  所以就算裝了 Office，只要沒啟用、裝不完整或被擋，就照樣用 LibreOffice。
- 試轉結果記在 `%LOCALAPPDATA%\Filee\msoffice-check.json`。Office 改版就重測，失敗的隔天重測。
- 個別檔案 Office 轉不了時，自動改用 LibreOffice 轉，例如 Excel 要輸出 PDF/A。
- 安全措施：
  - 巨集一律停用，連結不更新，檔案以唯讀開啟。
  - 有密碼的檔直接失敗，不會跳出密碼視窗卡住。
  - 超過 5 分鐘沒反應就結束它自己開的 Office。
  - 你正在用的 PowerPoint 不會被關掉。
- 不想用 Microsoft Office：設環境變數 `FILEE_MSOFFICE=off`。

**每週自動更新**：Filee Lite 常駐時一週檢查一次，在背景（不開視窗、較低優先權）更新兩樣東西：

- Filee 自己下載的引擎
- 內建的 7-Zip

版本與 SHA-256 以 Filee 最新版公布的清單為準，下載網址必須跟原本是同一家發行者，否則不採用。失敗的隔天重試。

- 紀錄在 `%LOCALAPPDATA%\FileeLite\logs\engine-update.log`。
- 要關掉：安裝時不勾這個選項，或把 `%LOCALAPPDATA%\FileeLite\engine-update.json` 裡的 `enabled` 改成 `false`。
- 自己用 winget 或安裝程式裝在系統上的 LibreOffice 等軟體，由它們各自的更新機制負責，Filee Lite 不會去動。
- 裝成「所有使用者」（Program Files）時，內建 7-Zip 沒有寫入權限，要等重裝新版 Filee Lite 才會更新。

## 目前狀態（2026-10-29）

- 安裝檔：`dist\FileeLite-2.2.0-lite.3-Setup.exe`（117.8 MB，對應上游 Filee 2.2.0）。`build_release.py` 一條指令 PASS，
  `cargo test` 225 個全過。上游補丁版的 hosted 模式、CLI 轉檔、自動轉換資料夾、轉換紀錄都真的跑過。
- 2026-10-29 早上加了以下六項，都用封裝好的 stage 實測過：
  - Microsoft Office 引擎：Word、Excel、PowerPoint 轉 PDF 都由 Office 產出，關掉 Office 後退回 LibreOffice。
  - 每週引擎更新：7-Zip 下載、寫入版本標記，第二次判定已是最新。
  - 「解壓縮」顯示沒有引擎的錯誤已修好。
  - 安裝選項全部預設勾選。
  - 沒跑過原版 Filee 的新電腦，環形選單是空的：已修好（第一次啟動時寫出上游的預設選單）。
  - PDF 選單的「合併 PDF」換成「轉成 Office」群組（DOCX、XLSX、PPTX）。PDF 轉 Excel 只抓有格線的表格。
- 還在實機驗證中：拖曳與環形選單、Ctrl+Alt+Space、系統匣、通知、右鍵、常駐記憶體、安裝流程。
- 已知限制：瀏覽器拖「連結」不會下載（直接給檔案的 Chrome／Edge 圖片、Firefox 都可以）；`.lnk` 不展開；
  上游的角落通知（更新、回饋、缺字型）不做；自動轉換資料夾不支援 `/正規表達式/`（glob 可以）；取消會直接結束轉檔程式。

## 上游出新版時

```powershell
python 工具\update.py          # 有新版就自動：抓 → 差異報告 → 換補丁 → 重編 → 新安裝檔
```

- 結束碼 0：新安裝檔在 `dist\`，可以直接帶去別台裝。
- 結束碼 2：安裝檔也建好了，但上游改了 Filee Lite 移植過的地方（觸發、選單、通知…），報告
  `docs\upstream-diffs\<舊>_to_<新>.md` 第 1 節列出要看的檔。

![閉環](docs/架構圖/PNG/閉環流程圖.png)

開發環境需要：Rust（cargo）、.NET 10 SDK、Inno Setup 6、Python 3、Git、Node.js（只有畫圖用）。

## 資料夾

| 路徑 | 內容 |
|---|---|
| `shell\` | Rust 原始碼（filee-lite） |
| `patches\hosted.patch` | 對上游的小補丁（讓上游 Filee 能被「臨時開、用完結束」） |
| `工具\` | `update.py`（閉環）、`build_release.py`（建置）、字型下載、300 DPI、A2 畫圖渲染 |
| `packaging\` | 安裝程式腳本 |
| `docs\` | 研究、移植規格、升版差異報告、架構圖（A2 300 DPI 的 PNG／PDF、HTML，含兩個動態版） |
| `dist\` | 產出的安裝檔 |

## 授權

MIT。這是非官方的改版，與 Filee 原作者及 Microsoft 無關。Filee 本體由 Filee contributors 以 MIT 授權；安裝檔內的 `licenses\` 附上游授權、第三方聲明與補丁全文。
