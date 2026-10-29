# 03 — Toast、角落通知、系統匣、主題、多語系 規格

範圍：Filee v2.2.0 上游（Avalonia 12.1.3 / .NET 10）中，Rust 常駐程式要自己畫的視覺元件：**toast 進度卡片、角落通知卡片、系統匣、主題（色票／字型／圓角）、多語系**。donut 本體由另一份規格負責，本檔只列 donut 會用到的主題色與 i18n key。

## 0. 閱讀約定

- 路徑縮寫：`A/` = `upstream/src/Filee.App/`，`C/` = `upstream/src/Filee.Core/`，`D/` = `upstream/docs/`。行號為上游原始檔實際行號（已逐檔核對）。
- DIP = Avalonia 的裝置無關像素（100% 縮放時 = 1 px）。「px」指實體像素。
- 可信度標記：**〔高〕** 直接來自上游原始碼；**〔中〕** 來自原始碼的推論；**〔低〕** 來自 Avalonia 內建（上游 repo 內沒有原始碼，未能驗證）或我的既有知識，實作時要實機比對。
- 「未找到」＝已在列出的位置搜尋過、確定沒有。
- 上游 repo 只有 `src/` 與 `docs/`；字型檔、`build/fetch-fonts.ps1` 都不在（見 §4.7）。

---

## 1. Toast（進度／結果卡片）

### 1.1 視窗與位置

| 項目 | 值 | 出處 |
|---|---|---|
| 視窗 | 無邊框、背景透明（`TransparencyLevelHint=Transparent`）、`Topmost`、不進工作列、`ShowActivated=False`（顯示時不搶焦點）、不可調整大小 | `A/Views/ToastWindow.axaml:9-17` 〔高〕 |
| 寬度 | **360 DIP**（視窗寬，含左右各 12 的透明邊）；高度 `SizeToContent=Height` 隨內容 | `ToastWindow.axaml:9-10` 〔高〕 |
| 視窗內邊距 | ItemsControl `Margin=12`（四邊）。**卡片實際寬 = 360 − 24 = 336 DIP**。這圈透明邊是留給卡片陰影的 | `ToastWindow.axaml:75` 〔高〕 |
| 卡片間距 | StackPanel `Spacing=8`（垂直） | `ToastWindow.axaml:77-79` 〔高〕 |
| 排序 | 新 job 插在最上面（`ToastJobs.Insert(0, vm)`）；視窗**貼右下角錨定**，job 增加時整個視窗向上長 | `A/Services/ConversionService.cs:90`、`ToastWindow.axaml.cs:20,78-88` 〔高〕 |
| 螢幕 | **只用主螢幕**（`Screens.Primary`）的**工作區**（`WorkingArea`，已扣掉工作列） | `ToastWindow.axaml.cs:78-88` 〔高〕 |
| 位置公式 | `x = WorkArea.Right − 視窗寬px − 12`；`y = WorkArea.Bottom − 視窗高px − 12`。視窗寬高 = `ceil(DIP × 螢幕縮放)`。**這個 12 是實體 px，不乘縮放**（上游小瑕疵；內側透明邊 12 DIP 有乘縮放）。100% 縮放時卡片右／下緣離工作區邊 = 12 + 12 = 24 px；150% 時 = 12 + 18 = 30 px | `ToastWindow.axaml.cs:14,78-88` 〔高〕 |
| 重新定位時機 | 每次視窗尺寸改變（`SizeChanged`）與顯示時 | `ToastWindow.axaml.cs:20,70` 〔高〕 |
| 顯示／隱藏 | 視窗在「`ToastJobs` 非空」時 `Show()`、空時 `Hide()`；變更在 UI thread `Post` 之後才評估（避免重複卡片，issue #60） | `ToastWindow.axaml.cs:60-76` 〔高〕 |
| 縮放 | **不套用 UiZoom**（視窗大小 70–180% 的設定對 toast 與所有角落通知無效，永遠 100%，只吃螢幕 DPI） | `D/ARCHITECTURE.md:255-258` 〔高〕 |
| 高度上限 | 未找到（沒有 max height、沒有捲動；job 很多時會超出螢幕）。搜尋：`ToastWindow.axaml*` | 〔高〕 |
| 其他限制 | toast 不受 `Paused` 影響（暫停只停觸發手勢）；未找到任何依 paused 隱藏 toast 的程式（搜尋：`ToastWindow*`、`ConversionService.cs`） | 〔中〕 |

### 1.2 卡片版面（每個 job 一張）

`Border.card`（`A/Styles/Soft.axaml:125-132`）：背景 `SurfaceBrush`、邊框 `SubtleBorderBrush` 1px、圓角 `SoftRadius`（預設 16）、陰影 `CardShadow`；toast 把 Padding 覆寫成 **14,12**（左右 14、上下 12；`ToastWindow.axaml:83`）。卡片內 StackPanel `Spacing=8`（`:84`）。由上而下：

1. **標題列**（`:85-91`）：右側 Close 按鈕（`Button.icon`：透明底、無邊框、`Padding=4`，內含 12×12 的 `Icon.Close` PathIcon，對齊上緣；無障礙名稱 `common.close`）；左側標題 `TextBlock`，字重 SemiBold、單行、尾端 `…` 截斷、垂直置中。標題文字 = `toast.converting` 以 {0}=（單檔 → 檔名；多檔 → `home.files` 的「N 個檔案」）、{1}=preset 顯示名稱（`A/ViewModels/JobViewModel.cs:30-31`）。zh-TW 範例：`foo.png → WebP`。
2. **下載徽章**（`:92-99`，僅 `IsDownloading`）：見 §1.4。
3. **進度條**（`:100`，僅 `IsRunning`）：`Value=Progress`、`Maximum=100`、高 6、圓角 999（全圓）（`:68-72`）。軌道／填色為 Avalonia Fluent 預設，填色由 accent 衍生 〔低〕——Filee 沒覆寫 ProgressBar 色（搜尋 `Styles/*.axaml`，只有 `ProgressBar.soft` 的尺寸與圓角，`Soft.axaml:195-199`）。建議：軌道 = `SurfaceAlt`，填色 = `Accent`，實機截圖比對。
4. **狀態／按鈕列**（`:101-118`，DockPanel）：右側按鈕群（水平、間距 6，順序由左至右：**取消**、**重試**、**檢視（開啟資料夾）**），左側狀態文字（`TextBlock.caption`：12px、`TextSecondaryBrush`、垂直置中）。按鈕 Padding = 10,4（`:103,104,110`）。
5. **錯誤明細**（`:119-120`，僅 `HasErrors`）：`caption danger`（12px、`DangerBrush`）、自動換行、**最多 4 行**。每個失敗檔一行：`{檔名} — {原因}`（`JobViewModel.cs:113-114`；原因 = `FailureText.Reason`：`loc[ErrorKey ?? "error.conversion_failed"]` ＋ （若有 detail）` ({detail})`；`A/ViewModels/FailureText.cs` 第 13-14 行）。檔名用 `Path.GetFileName`。
6. **備註**（`:121-122`，僅 `HasNotices`）：`caption`、換行、最多 4 行；每個「完成但有備註」的檔一行：`{檔名} — {備註}`；備註 = `loc[NoticeKey]`，若有 `NoticeReasonKey`／`NoticeDetail` 則加括號 `(原因: 細節)`（`JobViewModel.cs:123-126`、`C/Localization/ILocalizer.cs:32-38`）。
7. **缺字型列**（`:123-128`，僅有缺字型）：右側「取得字型」按鈕（Padding 10,4；僅當至少一個字型有已知網址，`CanGetFonts`）、左側 `caption` 文字 `toast.missing_fonts`（最多 3 行、右邊距 8），內容 = 去重（不分大小寫）、依文化排序、以 `, ` 連接的字型家族，在 RTL 語言外包 LRM（`JobViewModel.cs:128-135`）。點「取得字型」：開瀏覽器到**第一個有網址的字型**的頁面（`ToastWindow.axaml.cs:31-37`）。網址規則（`A/Services/FontSources.cs` 第 9-40 行）：
   - 在 `NoPublicFontPage` 清單（맑은 고딕、Malgun Gothic、함초롬바탕、HCR Batang、함초롬돋움、HCR Dotum、MS Mincho、MS Gothic、MS PMincho、MS PGothic、Yu Mincho、Yu Gothic、Wingdings 1-3、Webdings、Symbol、OpenSymbol）→ 無網址。
   - Microsoft 字型（Arial、Calibri、Calibri Light→calibri、Cambria、Cambria Math→cambria-math、Courier New→courier-new、Tahoma、Times New Roman→times-new-roman）→ `https://learn.microsoft.com/en-us/typography/font-list/{slug}`。
   - Helvetica Neue → `https://support.apple.com/en-us/108939`；Lohit Devanagari → `https://github.com/pravins/lohit`。
   - 其餘 → `https://fonts.google.com/?query={URL編碼的家族名}`。
8. **「請求支援此格式」按鈕**（`:129-131`，僅 `CanRequestFormat`；靠左、Padding 10,4）：點擊開瀏覽器到 `{RepositoryUrl}/issues/new?template=format_request.yml&title={"[Format]: " + ".ncm"}&extension={".ncm"}`（兩個參數皆 URL 編碼；只送副檔名不送檔名；一次最多 3 個副檔名）（`A/Services/FormatRequest.cs:11,17-24,30-36`、`ToastWindow.axaml.cs:25-29`）。`RepositoryUrl` 預設 `https://github.com/KnifeLemon/Filee`（`A/Services/UpdateService.cs:74-77`）。出現條件：job 已結束、有失敗檔且 `ErrorKey == "error.unsupported_source"`，且其副檔名 `FormatRegistry.Detect` 不認得、不是資料夾、非空（`JobViewModel.cs:118-121`）。

按鈕樣式：一般按鈕 = Fluent Button + Soft 覆寫（圓角 `SoftRadiusSmall`、背景 `SurfaceBrush`、hover `SurfaceAltBrush`、按下 `#E6E3DA`／`#3A3A36`、邊框 `#29000000`／`#38FFFFFF`；`Soft.axaml:33-38,56-61,166-174`）；按下時 `scale(0.95)`、過渡 0.22s BackEaseOut（`:175-177`）。`Classes="accent"` 的按鈕（只有「檢視」與通知卡主按鈕）＝ **Fluent 內建 accent 按鈕**，Filee 沒有自己的樣式（搜尋 `Styles/*.axaml` 無 `.accent` selector），實機（`Filee 轉換引擎安裝.png` 的「更新到 2.2.0」）看到的是 accent 色底＋白字 〔低〕：hover／pressed 色由 `SystemAccentColorLight1/Dark1…` 衍生（見 §4.3），前景色本檔無法確認，建議白字（accent 亮度 > 0.6 時用 `#1F1E1D` 較保險）。

「檢視」按鈕與「重試」按鈕左側各有 13×13 的 PathIcon（`Icon.Folder`、`Icon.Refresh`），圖示與文字間距 6（`:105-114`）。Icon 路徑資料見 §7.3。

### 1.3 狀態矩陣

`JobState` 有六種：`Queued、Running、Completed、CompletedWithErrors、Failed、Cancelled`（`C/Conversion/ConversionJob.cs:7-15`）。`Refresh()` 邏輯在 `JobViewModel.cs:104-146`。進度 = 各檔進度平均 ×100（`ConversionJob.cs:106`）；`IsRunning = state ∈ {Queued, Running}`。

| 狀態 | 狀態文字（`Status`） | 進度條 | 取消 | 重試 | 檢視 | 其他 | 自動消失 |
|---|---|---|---|---|---|---|---|
| 進行中（Queued/Running） | `"{progress:0}%"`（四捨五入到整數，例 `42%`） | 顯示 | 顯示 | — | 若已有輸出檔（`Outputs.Any()`）即顯示 | 下載徽章（瀏覽器來源檔）；錯誤／備註／缺字型即時顯示 | 否 |
| 成功（Completed） | `toast.done`「完成」 | 隱藏 | — | — | 有輸出才顯示 | 備註、缺字型 | **5 秒**後移除 |
| 全部跳過（Completed 且所有檔 `Skipped`） | `toast.skipped`「已跳過（檔案已存在）」 | 隱藏 | — | — | 視 `Outputs` 是否含檔；`Skipped` 檔的 `Outputs` 是否填入：**未確認**（搜尋 `JobQueue.cs` 未深入） | — | 5 秒 |
| 部分失敗（CompletedWithErrors） | `toast.done_errors` {0}=失敗數 {1}=總檔數，例「3 個中有 1 個失敗」 | 隱藏 | — | 見下 | 有輸出才顯示 | 紅字錯誤明細、可能的「請求支援此格式」、備註、缺字型 | **15 秒** |
| 失敗（Failed） | `toast.failed`「轉換失敗」 | 隱藏 | — | 見下 | 通常無 | 紅字錯誤明細 | **15 秒**（只要有失敗檔） |
| 取消（Cancelled） | `toast.cancelled`「已取消」 | 隱藏 | — | 見下（只有「失敗」檔才算） | 有輸出才顯示 | 取消的檔不是 `Failed`，所以通常沒有紅字 | 5 秒（無失敗檔時） |
| 缺字型（正交，疊在以上任何結束狀態） | 不變 | — | — | — | — | 缺字型列（§1.2-7） | 隨該狀態 |

細節：
- **自動消失計時**：job 結束時（`JobFinished`）才開始，成功／無失敗 = `KeepSuccess` **5 s**，`HasErrors` = `KeepErrors` **15 s**（`ConversionService.cs:20-21,225-226`）；計時不會因滑鼠 hover 暫停（未找到 hover 暫停邏輯，搜尋 `ToastWindow*`、`ConversionService.cs`）。計時到後 `Remove(vm)`：整張卡片直接消失，**沒有離場動畫**。最後一張消失 → 視窗 `Hide()`。
- **重試（`CanRetry`）** = 有提供重試回呼 且 非進行中 且 有失敗檔 且（job 沒有 Fetcher，或失敗檔中仍有存在於磁碟的檔）（`JobViewModel.cs:116-117`）。按下 → 移除本卡片，只對「失敗檔」以同一 preset 重跑，產生**新 job 卡片插在最上面**（`ConversionService.cs` `RetryFailed` ≈166-172；`Retry` ≈105-123）。來源已被刪除的檔會被排除；歷史紀錄裡沒存 PDF 密碼，會從 preset 補回。
- **檢視（`OpenFolderCommand`）** = 對 `Outputs.First()` 呼叫平台的 `RevealInFileManager`（Windows = 開檔案總管並**選取該輸出檔**）（`JobViewModel.cs:148-154`）。**沒有「開啟檔案」按鈕**；多輸出時只選第一個。
- **關閉（X）**：`Dismiss` → 立即從清單移除（`JobViewModel.cs:159-160`、`ConversionService.cs` `Remove` ≈244-250）。**對進行中的 job 只是隱藏卡片，不會取消轉檔**；之後仍會寫入歷史。要停止請按「取消」（`Job.Cancel()`，`JobViewModel.cs:156-157`）。
- **進行中時的取消**：只改 job 狀態，不彈確認。
- **與 donut 的交接**：從 donut 啟動的 job 先在 donut 的進度環顯示，**不進 toast**（`showInToast:false`）；以下任一情況才交給 toast：進行超過 **5 秒**（`HandOffAfter`）、job 失敗（進度環結束後由 toast 說明原因）、使用者又開了新的 donut 手勢（`A/Services/RadialController.cs:49,457-472,489-511`）。Cancelled／Completed 不交給 toast。自動轉換資料夾（watch folder）與重試的 job 直接進 toast（`ConversionService.cs:179-197`；`Start` 預設 `showInToast=true`）。
- **回饋卡**：成功（無失敗檔、所有檔皆 `Done`）累計 3 次（`FeedbackAfterConversions=3`）後，在該 toast 消失後再 1 秒（= 結束後 6 秒）觸發一次 `FeedbackDue`，永久只一次（`FeedbackPromptShown`）（`ConversionService.cs:24,225-231,≈233-242`）。見 §2。

### 1.4 下載徽章（瀏覽器拖入的檔案先下載時）

- 顯示條件：job 有 `Fetcher`（從瀏覽器拖入網頁圖片等）且正在儲存檔案。`JobViewModel` 建構時啟動 250 ms 計時器重寫徽章文字；job 結束後停止（`JobViewModel.cs:33-39,56-66`；`A/ViewModels/DownloadBadge.cs:15,18,21`）。
- 文字：`download.badge`「{0}/s · {1}」，{0} = 速度（近 **1 秒**平均，`Window=1s`）、{1} = `已下載 / 總量`；總量未知時只顯示已下載。多檔（`Count>1`）時改用 `download.badge_files`「{0} · 第 {1}/{2} 個檔案」，{1}=`min(已存+1, 總數)`。**速度要下載滿 300 ms（`Settle`）且速率 > 0 才首次顯示**（快速複製永遠不會出現徽章）；一旦顯示過，之後即使 `Update` 回傳 null 也保留最後的文字（讓縮小動畫不空白）（`DownloadBadge.cs:30-37`；`JobViewModel.cs:50-54,61-63`）。
- 數量格式 `FormatAmount`（`A/Services/EngineDownloadService.cs:151-157`）：≥1e9 → `x.xx GB`；≥1e8 → `x MB`（0 位小數）；≥1e6 → `x.x MB`；其餘 `x KB`（`bytes/1000`，下限 0，0 位小數）。皆為十進位（1000）制，小數點用目前文化。`Amounts`：已下載與總量同單位時只寫一次單位，例 `12.3 / 48.0 MB`；不同單位寫兩個，例 `850 KB / 1.20 GB`（`DownloadBadge.cs:47-62`）。
- 外觀：`Surface` 底、**accent 1px 邊框**、`SoftRadius` 圓角、Padding 8,2、文字 11px SemiBold、靠左；Margin 下方 −4（讓它貼近進度條）、動畫原點左下（`ToastWindow.axaml:92-99`）。
- 動畫：出現時 0.22s `BackEaseOut`，opacity 0→1、scale 0.6→1（`ToastWindow.axaml:43-58`）。下載完成（`IsDownloading=false`）就直接 `IsVisible=false`（toast 版沒有「縮出去」動畫；donut 版有，見 donut 規格）。

### 1.5 動畫

| 動畫 | 參數 | 出處 |
|---|---|---|
| 新卡片進場 | 0.35 s，`BackEaseOut`，`FillMode=Forward`；opacity 0→1，同時 Margin `40,0,-40,0` → `0`（**從右邊往左滑入 40 DIP**，因為用 Margin 模擬位移） | `ToastWindow.axaml:19-31` 〔高〕 |
| 減少動態（`.reduced-motion`） | 0.001 s，只把 opacity 設為 1（無滑動、無縮放）。徽章同理 | `ToastWindow.axaml:33-41,59-67` |
| 離場 | **無**（直接移除；視窗直接 Hide，沒有淡出）（搜尋 `ToastWindow.axaml*`） | 〔高〕 |
| 視窗顯示 | 無淡入（Show 後第一張卡片自己做進場動畫） | 〔中〕 |
| 按鈕按下 | `scale(0.95)`，0.22 s `BackEaseOut` 過渡；`.reduced-motion` 時停用 | `Soft.axaml:166-177,560-565` |

`Motion.Enabled` 為 false 的條件：設定 `theme.reduceMotion == true`；若為 `null` 則跟隨系統 `SPI_GETCLIENTAREAANIMATION` 為關（`A/Services/Motion.cs`、`ThemeService.cs:163`、`Filee.Platform.Windows/WindowsPlatformServices.cs:247-248`）。`BackEaseOut` = 標準 easeOutBack（overshoot 1.70158）〔低〕。

---

## 2. 角落通知（corner notices）

共 **5 種**（不含更新下載的對話框 `UpdateProgressWindow`、關閉詢問對話框 `CloseAskWindow`，那兩個是一般視窗，屬設定程式範疇；本檔不涵蓋）。共通：

- 視窗屬性與 toast 相同（透明、Topmost、不進工作列、不搶焦點、不可調整大小、不吃 UiZoom），寬度各不同，高度隨內容；**卡片 = `Border.card`，`Margin=12`、`Padding=16,14`**（其餘同 §1.2 的卡片樣式）。
- 位置 = 主螢幕工作區右下，同 §1.1 公式（右、下各 12 px），再減 `BottomOffset`（見下）。
- 版面骨架（除引擎更新外相同）：左側 **36×36 的 `filee.png`**（`Margin=0,2,12,0`，靠上；即 Filee 圖示，512×512 縮小）；右側垂直堆疊 `Spacing=8`：**標題列**（右 Close 按鈕 `Button.icon` Padding 4 + 12×12 `Icon.Close`；左標題 SemiBold、換行、垂直置中）→ 內文（`caption`，換行）→ 按鈕列（上方 `Margin 0,2,0,0`；按鈕 Padding **12,5**，間距 8；`feedback`／`elevation` 用 WrapPanel，按鈕 `Margin=0,0,8,6`）。
- 沒有自動消失計時（只有使用者按關閉或動作按鈕才關）、沒有進場動畫（`Motion.Track` 只切換 reduced-motion class，但檔內沒有動畫樣式）〔高〕：搜尋 `Views/*NoticeWindow.axaml`。
- 沒有淡入淡出；`Close()` 即銷毀。
- 疊放：**只有兩處有互相避讓**——更新通知建立當下若 toast 正在顯示，`BottomOffset = ceil(toast.Bounds.Height × scaling)`（只在建立當下算一次，之後 toast 增減不會更新；`A/Services/WindowService.cs:503-520`）；引擎更新通知若更新通知可見，`BottomOffset = 更新通知的高`（`:578-602`）。**回饋、系統管理員、格式請求三張卡完全不避讓**，會直接蓋在 toast／彼此上面（`FeedbackNoticeWindow.axaml.cs:34-43` 等；搜尋 `BottomOffset` 只出現在 `UpdateNoticeWindow`、`EngineUpdateNoticeWindow`）。這是上游現狀，Rust 版可自行改善（建議統一一個堆疊管理器）。
- 同類通知再觸發：更新、引擎更新、格式請求會先 `Close()` 舊的再開新的；回饋只在尚無舊的時才開；系統管理員每次執行只開一次（`WindowService.cs:503-643`）。

### 2.1 更新通知（UpdateNoticeWindow）

- 寬 **360**。檔案：`A/Views/UpdateNoticeWindow.axaml`（版面 16-49）、`.cs`。
- 觸發：`UpdateService.UpdateFound`（啟動後 20 秒開始定期檢查：`App.axaml.cs:239`）。等級由 release notes 的最高等級決定（`UpdateService.cs:190`）：**Required 每次啟動都顯示**；**Recommended 每個版本只顯示一次**（`NotifiedUpdateVersion` 去重）；**Optional 不顯示**（只在側欄顯示按鈕）（`App.axaml.cs:226-234`）。
- 內容：標題 `update.notice_title`「Filee {0} 已發布」；**等級膠囊**（圓角 999、Padding 8,2、字 11.5 SemiBold）：Required = `DangerBrush` 底＋純白字；Recommended = `AccentSoftBrush` 底＋`AccentStrongBrush` 字；Optional = `SurfaceAltBrush` 底＋`TextSecondaryBrush` 字（`UpdateNoticeWindow.axaml.cs:68-75`）；文字 `update.level.{Level}`；下方一行 `update.level_hint.{Level}`（caption）；更新內容清單（每項一列，`•` 項目符號 12.5px，最多 **6 行**，超過加 `update.more_changes`「… 另外 N 項」；`UpdateTexts.cs:37-44`）；狀態行 caption = 進行中的狀態句，否則是說明句；下載中顯示進度條（`Maximum=100`）；按鈕：accent「下載圖示 + `update.install`（已安裝版）／`update.download`（可攜版）」、「`update.later`稍後」；進行中（`IsBusy`）隱藏按鈕列。
- 動作：已安裝版 → 開始下載並安裝（卡片留著顯示進度）；可攜版 → 開 release 頁並關閉。
- 狀態句（`A/Services/UpdateTexts.cs:24-31`）：Downloading → `update.downloading`「正在下載更新… {0}%」；Installing → `update.installing`；Failed → `update.failed`；Cancelled → `update.cancelled`；否則 `update.notice_text`（已安裝）或 `update.notice_text_portable`（可攜）。
- **Rust 版建議**：v1 不實作（更新由上游 Filee.exe 負責 or 之後再說；見 §7.5）。

### 2.2 引擎更新通知（EngineUpdateNoticeWindow）

- 寬 **360**。`A/Views/EngineUpdateNoticeWindow.axaml:16-43`。
- 觸發：啟動後 **25 秒**（`App.axaml.cs:240`）、`CheckForUpdates` 開啟、且使用者已下載的引擎有比本版釘選版本更舊者，且該引擎版本與 `NotifiedEngineVersions[套件id]` 不同（每個新版本只通知一次；`App.axaml.cs:247-259`）。
- 版面：左側 **36×36 圓形**（圓角 999）、底 `AccentSoftBrush`，中央 18×18 `Icon.Engine`（色 `AccentStrongBrush`）；標題 `engines.update_notice_title`；caption = 引擎清單 `名稱(去掉「 (」之後) 版本` 以 ` · ` 連接，例 `FFmpeg 8.0 · LibreOffice 26.8`（`WindowService.cs:582`）；按鈕：accent「下載圖示＋`engines.update`」、「`update.later`」。
- 動作：更新 → 排入所有過期引擎的下載並開啟設定頁「engines」；關閉卡片。

### 2.3 回饋卡（FeedbackNoticeWindow）

- 寬 **360**。`A/Views/FeedbackNoticeWindow.axaml:16-37`。
- 觸發：第 3 次完全成功的轉換後（見 §1.3）、一次性。
- 內容：標題 `feedback.notice_title`、內文 `feedback.notice_text`（含 ⭐ 與換行）；按鈕：accent「`feedback.star`」→ 開 `RepositoryUrl`；「`feedback.issue`」→ 開 `{RepositoryUrl}/issues/new/choose`（`UpdateService.cs:84`）；兩者點完都關閉卡片。

### 2.4 系統管理員通知（ElevationNoticeWindow）

- 寬 **380**（唯一不是 360）。`A/Views/ElevationNoticeWindow.axaml:16-39`。
- 觸發：Filee 以系統管理員身分執行、而 Explorer 沒有（`ElevationService.ShellCannotReachFilee`），啟動後 **2 秒**，每次執行只一次（`App.axaml.cs:201-202`、`WindowService.cs:616-624`）。對 Rust 版：若 Rust 常駐程式本身被提權，同一問題（UIPI 擋 Explorer 拖放）依然存在，建議保留。
- 內容：標題 `elevation.notice_title`、內文 `elevation.notice_text`；失敗時多一行紅字 `home.elevated_restart_failed`（caption danger，預設隱藏）；按鈕：accent「`home.elevated_restart`」→ 以一般權限（透過 Explorer）重啟並結束自己，失敗則顯示紅字。

### 2.5 格式請求通知（FormatRequestNoticeWindow）

- 寬 **440**（最寬）。`A/Views/FormatRequestNoticeWindow.axaml:16-36`。
- 觸發：donut 在**全部都是不認得的副檔名**的檔案上關閉（沒有任何轉換、且 `ProfileSelector.DetectFormats` 為空）時，**同一個副檔名每次執行只提示一次**（`RadialController.cs:205-213`）。
- 內容：標題 `format_request.notice_title`（{0}=`.ncm` 或 `.ncm, .qmc`）、內文 `format_request.notice_text`（三行）；按鈕：accent「`format_request.request`」→ 開 §1.2-8 的網址並關閉；「`common.close`」。

---

## 3. 系統匣

### 3.1 圖示

| 項目 | 值 | 出處 |
|---|---|---|
| 圖示檔 | `src/Filee.App/Assets/Icons/filee.ico`（304,886 bytes；內含 **6 張 32bpp：256、64、48、32、24、16 px**，6 張都是未壓縮 BMP/DIB（頭 `0x28`，不是 PNG）） | `A/Services/TrayService.cs:21`；我讀取檔案標頭核對 〔高〕 |
| 其他圖示資源 | `filee.png`（512×512，通知卡左側用）、`filee.svg`（同目錄） | 〔高〕 |
| 狀態圖示 | **只有一個圖示，沒有 paused／更新中／忙碌等變體**。暫停只改 tooltip 與選單文字。搜尋：`TrayService.cs`、整個 `src` 搜尋 `Icon =` 與 `WindowIcon` | 〔高〕 |
| tooltip | 暫停中：`triggers.paused`「快捷操作已暫停」；否則 `tray.tooltip`「Filee：按住 {0} 拖曳檔案」，{0} = 第一個「已啟用且 Kind=Drag」的觸發手勢的修飾鍵，格式 `Ctrl + Alt`（`Ctrl`/`Alt`/`Shift`/`Win` 以 ` + ` 連接；順序固定 Ctrl→Alt→Shift→Win；沒有修飾鍵或找不到 → `—`） | `TrayService.cs:77-80`、`A/Services/GestureText.cs:9-17` 〔高〕 |
| tooltip 初值 | `"Filee"`（macOS 不能 null） | `TrayService.cs:39` |
| 顯示 | Windows 永遠顯示。`ShowMenuBarIcon` 設定**只對 macOS 生效**（`IsVisible = platform.SupportsDockVisibility ? ShowMenuBarIcon : true`；`SupportsDockVisibility` 只有 macOS 實作為 true）；Windows 上沒有「隱藏匣圖示」的設定 | `TrayService.cs:41,60`；`C/Platform/IPlatformServices.cs:16`、`Filee.Platform.MacOS/MacOSPlatformServices.cs:28` 〔高〕 |

### 3.2 互動

| 動作 | 行為 | 出處 |
|---|---|---|
| 左鍵點一下 | `Clicked` → `WindowService.ShowMain()`：開（或帶到前景）**設定主視窗** | `TrayService.cs:43` 〔高〕（Avalonia 在 Windows 以 `WM_LBUTTONUP` 觸發 `Clicked`；雙擊另有無額外處理：**未找到**） |
| 右鍵 | 顯示選單（Avalonia `NativeMenu`；在 Windows 由 Avalonia 以內部的 `TrayPopupRoot` 彈出視窗繪製，樣式屬 Avalonia Fluent，**上游 repo 沒有該類別原始碼**：搜尋 `TrayPopupRoot` 全 `src` 無結果）〔低〕 | `TrayService.cs:40` |
| 提示氣泡（balloon） | **未找到**（上游不使用 Windows 氣泡通知；所有通知都是 §1、§2 的自繪卡片）。搜尋：整個 `src` 搜尋 `Balloon`、`ShowNotification`、`ToastNotification` | 〔高〕 |

### 3.3 選單項目（由上到下）

動態項（條件成立才出現）放在**最上面**，其餘固定。`NativeMenuItem`，**沒有圖示、沒有勾選**。

| # | 項目 | 文字（i18n key → zh-TW） | 條件 | 動作 |
|---|---|---|---|---|
| 0（動態） | 更新 | 閒置：`tray.update`「更新到 {0}」{0}=最新版本；下載中：`update.downloading`「正在下載更新… {0}%」；安裝中：`update.installing_short`「正在安裝…」（`UpdateTexts.Short`，`UpdateTexts.cs:16-21`） | `updates.LatestVersion != null` 才存在；否則從選單移除 | `WindowService.StartUpdateAsync`：若正在忙則顯示更新進度對話框；否則開始下載＋顯示進度對話框（`WindowService.cs:526-537`） |
| 1 | 開啟 Filee | `tray.open`「開啟 Filee」 | 恆在 | `ShowMain()`（同左鍵） |
| 2 | 暫停／恢復 | `Paused=false` → `tray.pause`「暫停快捷操作」；`Paused=true` → `tray.resume`「恢復快捷操作」 | 恆在 | 切換 `settings.paused` 並存檔（`SaveSettings` 會觸發 `SettingsChanged` → 重新套用觸發設定與 tooltip） |
| — | 分隔線 | | | |
| 3 | 結束 | `tray.quit`「結束」 | 恆在 | `ApplicationLifetime.Shutdown()` |

文字更新時機：語言切換（`LanguageChanged`）、設定變更（`SettingsChanged`）、`UpdateService` 的相關屬性變更（`UpdateTexts.Affects`：`LatestVersion`、`Step`、`DownloadPercent`、`Notes`）（`TrayService.cs:46-53`）。

### 3.4 與匣相關的設定

- `closeAction`（settings.json）：`Ask | Tray | Quit`，預設 `Ask`（`C/Settings/AppSettings.cs:22-33,140-142`）。只影響**設定主視窗**的關閉按鈕／Alt+F4：`Tray` → 隱藏並釋放視窗、常駐匣；`Quit` → 結束整個程式；`Ask` → 彈 `CloseAskWindow`（`close.title`「讓 Filee 繼續執行嗎？」等，附「不再詢問」勾選，勾選後把答案存回 `closeAction`）（`A/Views/MainWindow.axaml.cs:38-93`）。程式內呼叫 `Close()`（非使用者）一律當 `Tray`。**在新架構中，設定視窗是臨時啟動的 Filee.exe，這個設定只與那個視窗有關；Rust 常駐程式沒有主視窗，不需要實作 `Ask` 對話框**。
- `paused`：`bool`，預設 false（`AppSettings.cs:182`）。除了選單文字／tooltip，暫停時選取手勢（`SelectionGesture`）會被忽略（`App.axaml.cs:96`）；其餘觸發行為由 `TriggerService.Apply(settings)` 處理（另一份規格）。
- 匣常駐結束時：`TrayService.Dispose()` 在 `desktop.Exit`（`App.axaml.cs:173`）。

---

## 4. 主題

### 4.1 設定欄位（settings.json 的 `theme`）

settings.json 是 camelCase、列舉為**字串**（`UseStringEnumConverter`）、允許註解與結尾逗號（`C/Settings/FileeJsonContext.cs:11-19`）；資料夾：Windows `%APPDATA%\Filee\`，或環境變數 `FILEE_DATA_DIR`（`D/CUSTOM-THEMES.md:3-8`）。Rust 解析要用容許註解／尾逗號的 JSON（如 JSONC／json5 類 crate），否則使用者手改過的檔會讀不進來。

```jsonc
"language": "auto",            // 或 en/de/tr/es/ja/ko/zh-CN/zh-TW/pl/hi/ru/ar/pt-PT/pt-BR/fr/ne/th
"theme": {
  "style": "Soft",             // "Soft" | "MaterialYou"            預設 Soft
  "mode": "System",            // "System" | "Light" | "Dark"        預設 System
  "accent": "#7F77DD",         // #RRGGBB                            預設 #7F77DD
  "presetKey": null,           // null 或 monochrome|lavender|peach|mint|rose|sky|amber|sage
  "cornerRadius": 16,          // 0..28（Math.Clamp）                預設 16
  "reduceMotion": null         // null=跟系統, true, false
},
"uiScale": 1,                  // 設定視窗縮放，toast/通知不套用
"paused": false, "closeAction": "Ask", "showMenuBarIcon": true
```
出處：`AppSettings.cs:9-20,36-58,131,140-142,167,182,262`。`theme.accent` 解析失敗 → 退回預設 `#7F77DD`（`ThemeService.cs:90-91`）。

### 4.2 style 與 mode

- `style`：`Soft`（預設；經典樣式）與 `MaterialYou`（M3 風格、由 accent 推導整套色票）。差異：MaterialYou 的色票由 §4.6 演算法產生（覆蓋 §4.3 的固定色）；視窗加 `.material-you` class → 按鈕改**全圓角**（`SoftRadiusPill`）＋字重 Medium（排除 `.icon`、`.navlink`、`.choice-card`；`Soft.axaml` ≈445-449）；卡片圓角仍 `SoftRadius`（`Soft.axaml:434-437`；toast 的 Padding 因為本地值優先而不變）。i18n：`theme.style.Soft`「Filee Soft」、`theme.style.MaterialYou`「Material You」。
- `mode`：`System` → `ThemeVariant.Default`（跟隨 OS）；`Light`、`Dark` 強制（`ThemeService.cs:77-82`）。**System 如何判斷**：交給 Avalonia 的 Win32 平台設定，實際讀 Windows「應用程式模式」（`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`，0 = 深色）〔低〕——上游 repo 內沒有這段（Avalonia 內部）。Rust 版自行讀該登錄值，並在 `WM_SETTINGCHANGE`（lParam = `"ImmersiveColorSet"`）時重讀。

### 4.3 Soft 樣式色票（預設，未被 theme.json 覆寫時）

出處 `A/Styles/Soft.axaml:14-62`。色碼為 `#RRGGBB` 或 `#AARRGGBB`（Avalonia 的 8 碼是 **ARGB**，Alpha 在前）。

| Key | Light | Dark | 用在 |
|---|---|---|---|
| `PageBrush` | `#F6F5F1` | `#1B1B19` | 視窗背景（toast 不用；設定視窗用） |
| `SurfaceBrush` | `#FFFFFF` | `#262624` | **卡片底**、按鈕底、徽章底 |
| `SurfaceAltBrush` | `#EFEDE6` | `#31312E` | 按鈕 hover、Optional 膠囊底、小面板 |
| `SubtleBorderBrush` | `#16000000`（黑 8.6%） | `#1CFFFFFF`（白 11%） | **卡片邊框** |
| `TextPrimaryBrush` | `#1F1E1D` | `#F1EFE8` | 內文、標題 |
| `TextSecondaryBrush` | `#6B6A66` | `#AEACA4` | `caption`（狀態文字等） |
| `DangerBrush` | `#C73A39` | `#F09595` | 錯誤文字、Required 膠囊底 |
| `SuccessBrush` | `#0F7A5A` | `#5DCAA5` | 成功（toast 本身沒用；donut 進度環可能用） |
| `WarningBrush` | `#9A5B0C` | `#EF9F27` | 警示 |
| `CardShadow` | `0 1 3 0 #12000000, 0 6 18 0 #0D000000` | `0 1 3 0 #40000000, 0 8 24 0 #33000000` | 卡片陰影，兩層，格式 `offsetX offsetY blur spread color` |
| `ButtonBackground` / `PointerOver` / `Pressed` | `#FFFFFF` / `#EFEDE6` / `#E6E3DA` | `#262624` / `#31312E` / `#3A3A36` | 一般按鈕底 |
| `ButtonBorderBrush` / `PointerOver` / `Pressed` | `#29000000` / `#3D000000` / `#3D000000` | `#38FFFFFF` / `#4DFFFFFF` / `#4DFFFFFF` | 一般按鈕邊框 |
| `AccentBrush` 系列（預設值） | `#7F77DD`、Soft `#2E7F77DD`、Faint `#177F77DD`、Strong `#5F59A6`、OnAccent `#FFFFFF` | 同（Dark 區塊也是這組；實際會被 `ApplyAccent` 覆寫） | 見 §4.4 |

其他常數：`SoftRadius 16 / SoftRadiusSmall 9 / SoftRadiusLarge 22`（預設，`Soft.axaml:66-68`；實際由 cornerRadius 重算，見 §4.5）；基礎字級 **14**（`Window` 樣式，`:83-90`）；`caption` 12px + `TextSecondary`（`:111-114`）；`danger` 文字 = `DangerBrush`（`:120-122`）。

**`theme.json` 覆寫鍵 ↔ 上列 key**（`ThemeService.cs:57-68`）：`page→PageBrush`、`surface→SurfaceBrush`、`surfaceAlt→SurfaceAltBrush`、`border→SubtleBorderBrush`、`text→TextPrimaryBrush`、`textSecondary→TextSecondaryBrush`、`danger→DangerBrush`、`success→SuccessBrush`、`warning→WarningBrush`。覆寫了 `surface` 時，`ButtonBackground` 也跟著換成它；覆寫了 `surfaceAlt` 時，`ButtonBackgroundPointerOver` 也換（`ThemeService.cs:169-179`）。**陰影、按下色、按鈕邊框、accent 系列都不能用 theme.json 覆寫**（不在 9 個鍵內）。

### 4.4 accent 如何衍生其他色（Soft 樣式）

`SoftPalette.ApplyAccent(accent)`（`A/Services/SoftPalette.cs:66-81`）：

| 輸出 | 公式 |
|---|---|
| `AccentColor` / `AccentBrush` | `accent` |
| `AccentSoftBrush` | `accent` 色、**brush Opacity 0.18**（結果 Alpha ≈ 0.18×255 = 46 = `0x2E`；預設 `#2E7F77DD` 即為驗證） |
| `AccentFaintBrush` | `accent`、Opacity **0.09**（≈ `0x17`） |
| `AccentStrongBrush` | `Shade(accent, −0.25)` |
| `OnAccentBrush` | `Luminance(accent) > 0.6` → `#1F1E1D`，否則 `#FFFFFF` |
| `SystemAccentColorLight1/2/3` | `Shade(accent, +0.15 / +0.30 / +0.45)` |
| `SystemAccentColorDark1/2/3` | `Shade(accent, −0.15 / −0.30 / −0.45)` |

- `Shade(c, a)`（`SoftPalette.cs:182-188`）：`a ≥ 0` → 每通道 `round(v + (255−v)·a)`；`a < 0` → `round(v·(1+a))`；保留 Alpha；`round` 為 .NET 預設 `Math.Round`（四捨五入到偶數，只在恰好 .5 時有差）。
- `Luminance(c) = (0.2126R + 0.7152G + 0.0722B)/255`（非線性 sRGB 近似；`:191`）。
- 驗證：accent `#7F77DD` → Strong = (127·.75=95.25→95=`5F`, 119·.75=89.25→89=`59`, 221·.75=165.75→166=`A6`) = `#5F59A6` ✓ 與預設值一致。
- `System*` 這些 key 是給 Avalonia Fluent 內建控制項（accent 按鈕、進度條、選取色）用的；Rust 自繪時只需要：`Accent`、`AccentSoft`、`AccentFaint`、`AccentStrong`、`OnAccent`，以及 accent 按鈕 hover = `SystemAccentColorLight1`（Dark 模式）／`Dark1`（Light 模式）、pressed = `Light2`／`Dark2` 〔低；Fluent 內部對應未驗證，建議對照 Filee 實機〕。
- **預設 preset（`presetKey`）**：非空且在清單內時，Light/Dark 各用成對的 accent，**忽略 `theme.accent`**（`ThemeService.cs:86-94,134-147`）。清單（`ThemeService.cs:35-45`）：

  | key | Light accent | Dark accent |
  |---|---|---|
  | `monochrome` | `#1F1E1D` | `#F1EFE8` |
  | `lavender` | `#7F77DD` | `#BCAEE5` |
  | `peach` | `#E07A70` | `#F29B91` |
  | `mint` | `#529B7D` | `#88D5B1` |
  | `rose` | `#E86483` | `#F79BB0` |
  | `sky` | `#5B8BE3` | `#98B8F6` |
  | `amber` | `#CB852A` | `#F0BA6A` |
  | `sage` | `#5A8E51` | `#8DC983` |

  `monochrome` 在 Soft 樣式走 `ApplyMonochromeAccent`（`SoftPalette.cs:87-104`）：accent = Light `#1F1E1D` / Dark `#F1EFE8`；OnAccent = Light `#FFFFFF` / Dark `#1F1E1D`；`AccentSoft` = accent Opacity Light 0.12 / Dark 0.18；`AccentFaint` = Opacity Light 0.05 / Dark 0.08；`AccentStrong` = accent 本身；System* 同上 Shade。
  自訂色票（theme page 的 8 個 swatch）：`#7F77DD #1D9E75 #D85A30 #378ADD #D4537E #BA7517 #639922 #5F5E5A`（`ThemeService.cs:23-24`）。

### 4.5 cornerRadius 的套用

`ApplyCornerRadius(r)`（`SoftPalette.cs:107-116`），`r = clamp(cornerRadius, 0, 28)`（預設 16，`SoftPalette.cs:18`）：

| 資源 | 公式 | r=16 時 | toast／通知用在 |
|---|---|---|---|
| `SoftRadius` | r | 16 | **卡片**、下載徽章 |
| `SoftRadiusSmall` | `round(r·0.55)` | 9 | **按鈕**（Soft 樣式）、ControlCornerRadius |
| `SoftRadiusLarge` | `round(r·1.35)` | 22 | （donut/設定頁） |
| `ControlCornerRadius` | `round(r·0.55)` | 9 | Fluent 控制項 |
| `OverlayCornerRadius` | r | 16 | |
| `SoftRadiusPill` | `round(r·2.5)` | 40 | MaterialYou 按鈕、徽章 |

固定 999（= 全圓）：進度條、等級膠囊、圓形引擎圖示底。Close 按鈕雖是 `Button`，但本身無邊框透明，圓角只影響 hover 時的底色形狀（hover 底：因 Background=Transparent，視 Fluent 而定，**〔低〕**，建議 hover 時畫 `SurfaceAlt`）。

### 4.6 MaterialYou 樣式演算法（`A/Services/MaterialYouPalette.cs`）

**只有 `style=MaterialYou` 時才用**，覆蓋 §4.3 的固定色（Light、Dark 各一套）。accent 來源：`presetKey` 命中則取該 preset 的 Light／Dark accent，否則兩者皆為 `theme.accent`。

```
(h,s,l) = RgbToHsl(seed)         // h,s,l 皆 0..1，函式見 MaterialYouPalette.cs:187-246（可直接移植，含 HslToRgb 與 round）
sat = clamp(s, 0.20, 0.85)
IsMonochrome(seed): s < 0.08  或  R=G=B  或  三通道兩兩差 ≤ 4   → 走固定單色色票（見下）
```
Light（`:81-107`）：`primary = l>0.60 ? HSL(h,sat,0.45) : seed`；`onPrimary = Luminance(primary)>0.55 ? #1F1E1D : #FFFFFF`；
Page = HSL(h, min(sat·0.18,0.12), 0.97)；Surface = `#FFFFFF`；SurfaceAlt = HSL(h, min(sat·0.22,0.16), 0.92)；SubtleBorder = primary 的 **Alpha 28**（`#1C`+RGB）；TextPrimary = HSL(h,0.10,0.12)；TextSecondary = HSL(h,0.15,0.38)；Danger `#BA1A1A`；Success `#1B6D24`；Warning `#825500`；CardShadow 同 §4.3 Light；Accent = primary；OnAccent = onPrimary；AccentSoft = HSL(h, min(sat·0.5,0.40), 0.90)（**不透明色**，非 Opacity）；AccentFaint = HSL(h, min(sat·0.35,0.25), 0.95)；AccentStrong = HSL(h, min(sat·0.7,0.60), 0.22)。

Dark（`:108-134`）：`darkPrimary = l<0.65 ? HSL(h, min(sat·0.75,0.65), 0.75) : seed`；`onPrimary = Luminance(darkPrimary)>0.45 ? #16141A : #FFFFFF`；
Page = HSL(h, min(sat·0.18,0.12), 0.08)；Surface = HSL(h, min(sat·0.20,0.14), 0.13)；SurfaceAlt = HSL(h, min(sat·0.22,0.16), 0.18)；SubtleBorder = darkPrimary 的 **Alpha 32**；TextPrimary = HSL(h,0.08,0.92)；TextSecondary = HSL(h,0.12,0.75)；Danger `#FFB4AB`；Success `#8CD692`；Warning `#FFBA2F`；CardShadow 同 §4.3 Dark；Accent = darkPrimary；AccentSoft = HSL(h, min(sat·0.5,0.38), 0.26)；AccentFaint = HSL(h, min(sat·0.30,0.20), 0.15)；AccentStrong = HSL(h, min(sat·0.7,0.55), 0.88)。

單色（monochrome seed，`:137-185`）：
| | Light | Dark |
|---|---|---|
| Page / Surface / SurfaceAlt | `#F7F6F2` / `#FFFFFF` / `#EEECE5` | `#161614` / `#222220` / `#2C2C29` |
| SubtleBorder | `#18000000`（Alpha 24） | `#1CFFFFFF`（Alpha 28） |
| TextPrimary / TextSecondary | `#1F1E1D` / `#6E6C67` | `#F1EFE8` / `#AEACA4` |
| Danger / Success / Warning | `#C73A39` / `#0F7A5A` / `#9A5B0C` | `#F09595` / `#5DCAA5` / `#EF9F27` |
| Accent / OnAccent | `#1F1E1D` / `#FFFFFF` | `#F1EFE8` / `#1F1E1D` |
| AccentSoft / Faint | accent Alpha 32 / 14 | accent Alpha 46 / 20 |
| AccentStrong | accent | accent |

HSL 轉換注意：`RgbToHsl` 在 `delta ≤ 1e-5` 時 h=s=0；`HslToRgb` 在 `s < 1e-5` 回傳灰；channel 以 `round(x·255)`（銀行家捨入）並夾在 0..255。移植時直接照抄函式即可。

### 4.7 字型與 fallback 順序

- 字級：基礎 14；標題 SemiBold 14；caption 12；徽章 11；等級膠囊 11.5；更新內容 12.5。
- UI 字型鏈 `FontFamilies(language)`（`SoftPalette.cs:122-132`），逗號分隔、依序 fallback（逐字元 fallback）：
  ```
  Noto Sans, Noto Sans Arabic, Noto Sans Devanagari, Noto Sans Thai,
  [僅 zh-TW/zh-HK/zh-Hant…: Microsoft JhengHei UI, Microsoft JhengHei, PingFang TC,]
  <內嵌 CJK ×3，順序依語言>,
  Segoe UI, Nirmala UI, Leelawadee UI,
  <系統 CJK ×3，順序同上>,
  Kohinoor Devanagari, Geeza Pro, Thonburi, sans-serif
  ```
  內嵌 CJK 與系統 CJK（`SoftPalette.cs:27-32`）：JP = `Noto Sans JP` / `Yu Gothic UI, Meiryo, Hiragino Sans`；KR = `Noto Sans KR` / `Malgun Gothic, Apple SD Gothic Neo`；SC = `Noto Sans SC` / `Microsoft YaHei, PingFang SC`。順序：語言以 `ja` 開頭 → JP,KR,SC；以 `zh` 開頭 → SC,KR,JP；其他 → KR,SC,JP。`Noto Sans` 放第一是因為「第一個字型負責畫截斷用的 `…`」（`:129`）。zh-TW 判定：`AppLanguages.IsTraditionalChinese`（`zh` 且含 `Hant/TW/HK/MO/CHT`、且不含 `Hans`；`AppLanguages.cs:52-61`）。
  **zh-TW 實際會畫**：Noto Sans（Latin/數字）→ Microsoft JhengHei UI（中文，因為排在內嵌 Noto SC 之前，所以是繁體字形）。
- **內嵌字型**：`fonts:Filee#Noto Sans`、`#Noto Sans JP/KR/SC/Devanagari/Arabic/Thai`（`A/Program.cs:305-309`，資源路徑 `avares://Filee/Assets/Fonts`）。**字型檔不在上游 repo**（由 `build/fetch-fonts.ps1` 下載並以 SHA-256 釘住；`D/ADDING-A-LANGUAGE.md:14`），我也檢查過 `A/bin/Release/.../Filee.dll` 內只有 `Assets/Icons/*`，沒有 `Assets/Fonts`。**未找到**字型檔，搜尋過：`upstream/**/*.ttf|*.otf`、`Filee.dll` 內嵌資源。授權 SIL OFL 1.1（`THIRD-PARTY-NOTICES.md:108`）；繁體中文沒有內嵌（`SoftPalette.cs:34-38`：多兩個 Noto CJK 檔要 +11 MB）。
- 安裝程式用的鏈 `SystemFontFamilies`（只含 Windows 系統字型：`Segoe UI, Nirmala UI, Leelawadee UI, [繁中: Microsoft JhengHei UI, Microsoft JhengHei, PingFang TC,] 系統 CJK ×3, sans-serif`；`SoftPalette.cs:138-146`）——**Rust 版若不想附字型，直接用這條鏈即可**（Windows 10 1809+ 都有，含粗體）。
- RTL：`ar` 時視窗 `FlowDirection=RightToLeft`，整個版面鏡射；圖片／PathIcon 不鏡射（`Soft.axaml:88-98`、`SoftPalette.cs:149-176`）；「→」箭頭在 RTL 語言換成「←」。判定 `CultureInfo.TextInfo.IsRightToLeft`。目前只有 `ar` 是 RTL。zh-TW 不需處理，但框架上要留好（`LeftToRight()` 在列表型文字外包 U+200E，`LocalizationService.cs:88-89`）。

### 4.8 `theme.json` 自訂主題格式

- 位置：與 settings.json 同資料夾，檔名 `theme.json`；**啟動時與每次設定存檔時重新讀取**（`D/CUSTOM-THEMES.md:3-8,31-32`；`App.axaml.cs:282`）。
- 格式：

  ```jsonc
  { "dark": { "page": "#101820", "surface": "#18222C", "surfaceAlt": "#22303C",
              "border": "#1CFFFFFF", "text": "#E8E4D8", "textSecondary": "#A8B0B8",
              "danger": "#F09595", "success": "#5DCAA5", "warning": "#EF9F27" },
    "light": { "page": "#F4F6F8" } }
  ```
- 規則（`C/Settings/CustomTheme.cs`）：`dark`、`light` 與其中每個色都選填；**變體名稱區分大小寫**（`"Dark"` 無效）、**色名不分大小寫**；值須符合 `^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$`（`#RRGGBB` 或 `#AARRGGBB`），否則忽略並寫 log；整份沒有任何可用色 → 視為沒有主題（回 null）；檔案 > **64 KB**（`MaxBytes`）→ 略過；讀取失敗 → 略過；檔案由使用者擁有，Filee 從不改寫（`CustomTheme.cs:22,25-26,42-78,80-98`）。覆寫只作用於對應變體（Light/Dark 的 `ThemeDictionaries`），在 `style=Soft` 與 `MaterialYou` 皆適用，且**覆寫優先於** MaterialYou 產生的色（`ThemeService.cs:109-110`）。
- 官方範例：`D/themes/{catppuccin,dracula,monokai,nord}.json`（各為 MIT 來源的改作，見 `D/themes/NOTICE.md`；Monokai 只有 dark，Light 模式時用內建 light 色）。

---

## 5. 多語系

### 5.1 檔案結構與載入

- 位置：`A/Assets/i18n/<code>.json`，共 17 個：`en, de, tr, es, ja, ko, zh-CN, zh-TW, pl, hi, ru, ar, pt-PT, pt-BR, fr, ne, th`（`A/Services/AppLanguages.cs:9-28`，此順序同語言選單順序）。
- 格式：**單層** `{"key": "文字", ...}`（UTF-8；`zh-TW.json` 1228 個 key，與 `en.json` key 集合完全相同，**沒有缺 key**；我用腳本比對過）。key 為 `區.名稱` 或 `區.名稱.子項`。檔案中的空行無意義；JSON 內沒有註解（標準 JSON）。
- 佔位符：.NET `string.Format` 風格 `{0}`、`{1}`（零起算），以 `CultureInfo.CurrentCulture` 格式化；格式化失敗（`FormatException`）時回傳**未代入的原字串**。文字內換行是 JSON 的 `\n`。測試保證各語言佔位符與英文一致（`D/ADDING-A-LANGUAGE.md:2-4`）。
- 查字串規則（`A/Services/LocalizationService.cs:40-43`）：當前語言 → **英文** → **key 本身**。
- 載入：`LoadStrings(code)` 讀 `avares://Filee/Assets/i18n/{code}.json`；`en` 常駐記憶體作 fallback（`LocalizationService.cs:25-29,150-157`）。語言切換時整批重新發佈，UI 即時更新（`:64-82`）。同時把 `CultureInfo.CurrentUICulture`／`DefaultThreadCurrentUICulture` 設成該語言。
- **`language = auto` 如何決定**（`LocalizationService.cs:96,99-144`）：
  1. 設定值若是 17 個代碼之一（不分大小寫）→ 直接用。
  2. 否則取「使用者讀 OS 的語言」= 程式啟動當下的 `CultureInfo.CurrentUICulture.Name`（**不是** `InstalledUICulture`；macOS 另有路徑）。
  3. 逐條比對，順序：`de`／`de-*`→de；葡語（`pt*`：region 為 BR（含 `pt-Latn-BR`、`ptbr`）→ pt-BR，其他 → pt-PT；`AppLanguages.cs:31-46`）；`tr`／`tr-*`→tr；`ja*`→ja；`ko*`→ko；`zh*`→（`IsTraditionalChinese` ? **zh-TW** : zh-CN；`zh-TW/zh-HK/zh-MO/zh-Hant*/zh-CHT` → zh-TW，`zh-Hans-TW` 等有 `Hans` → zh-CN，裸 `zh` → zh-CN）；`pl*`→pl；`es*`→es；`hi*`→hi；`ru*`→ru；`ar`／`ar-*`→ar；`fr`／`fr-*`→fr；`ne`／`ne-*`→ne；`th`／`th-*`→th；其餘 → **en**。
  Windows 上取 OS 顯示語言建議用 `GetUserPreferredUILanguages`（或 `GetUserDefaultUILanguage`）〔低：Avalonia 內部實作未驗證〕。
- 與函式相關的 i18n 約定：
  - preset 名稱：`DisplayName` = 使用者自訂 `name` → `nameKey` 對應字串 → 目標格式大寫（`ILocalizer.cs:40-43`）。
  - donut 簡稱：`{nameKey}.donut`（沒有則退回 DisplayName；`:46-53`）；群組環內簡稱：`{nameKey}.short`（`:60-67`）。
  - 失敗訊息的 `error.*` 與備註的 `notice.*` 來自 job 結果（`FileResult.ErrorKey`、`NoticeKey`、`NoticeReasonKey`）。**注意：新架構下 Rust 呼叫 `filee-cli.exe` 轉檔；CLI 的輸出是已翻譯好的人類可讀文字與 `--json`，不是 key**（`upstream/src/Filee.Cli/Cli.cs:100-215`：`ok/skip/stop/fail` 行與 JSON 的 `error`／`notice` 欄位都是已查表文字，且不含逐檔進度百分比；語言取決於 CLI 的 `host.Texts`，**未驗證**）。若要在 toast 顯示本地化的錯誤，Rust 需決定：直接用 CLI 的文字，或自行對照下表 key（CLI 若能輸出 key 則最佳）。這是**跨規格問題**，回報給主 agent。

### 5.2 元件用到的 key → zh-TW 字串

完整對照表見下（由 `zh-TW.json` 以腳本產出，逐字複製；`\n` 表示換行）。

#### A. Toast

`{0}` 等為佔位符。`toast.*` 出處 `JobViewModel.cs:31,104-146`、`ToastWindow.axaml`；`common.*`、`home.files`、`download.*`、`format_request.request` 同。

| key | zh-TW |
|---|---|
| `toast.converting` | {0} → {1} |
| `toast.done` | 完成 |
| `toast.done_errors` | {1} 個中有 {0} 個失敗 |
| `toast.failed` | 轉換失敗 |
| `toast.cancelled` | 已取消 |
| `toast.skipped` | 已跳過（檔案已存在） |
| `toast.open_folder` | 檢視 |
| `toast.cancel` | 取消 |
| `toast.missing_fonts` | 文件缺少字型：{0} |
| `toast.get_fonts` | 取得字型 |
| `common.close` | 關閉 |
| `common.retry` | 重試 |
| `common.cancel` | 取消 |
| `home.files` | {0} 個檔案 |
| `download.badge` | {0}/s · {1} |
| `download.badge_files` | {0} · 第 {1}/{2} 個檔案 |
| `format_request.request` | 請求支援此格式 |

#### B. 角落通知（5 種）

| key | zh-TW |
|---|---|
| `update.notice_title` | Filee {0} 已發布 |
| `update.level.Required` | 必要更新 |
| `update.level.Recommended` | 建議更新 |
| `update.level.Optional` | 可選更新 |
| `update.level_hint.Required` | 修復了影響轉換結果的問題，請務必更新。 |
| `update.level_hint.Recommended` | 修復了一些問題。 |
| `update.level_hint.Optional` | 新增功能，需要時再更新即可。 |
| `update.install` | 更新 |
| `update.download` | 下載 |
| `update.later` | 稍後 |
| `update.notice_text` | 點擊「更新」，Filee 會下載並安裝新版本，設定會保留。 |
| `update.notice_text_portable` | 這是可攜版，請下載新的 zip 並解壓縮覆蓋當前資料夾，設定會保留。 |
| `update.downloading` | 正在下載更新… {0}% |
| `update.installing` | 安裝程式正在執行，完成後 Filee 會重新啟動。 |
| `update.installing_short` | 正在安裝… |
| `update.failed` | 無法自動更新，請從 filee.sh 下載新版本。 |
| `update.cancelled` | 已取消更新，再次點擊「更新」即可安裝。 |
| `update.more_changes` | … 另外 {0} 項 |
| `engines.update_notice_title` | 已安裝的引擎有新版本 |
| `engines.update` | 更新 |
| `feedback.notice_title` | Filee 對你有幫助嗎？ |
| `feedback.notice_text` | 在 GitHub 上加個星號（⭐）能幫助更多人發現 Filee。
也歡迎回報問題或提出想法。 |
| `feedback.star` | 在 GitHub 上加星號 |
| `feedback.issue` | 回報問題 · 提出建議 |
| `elevation.notice_title` | Filee 正以系統管理員身分執行 |
| `elevation.notice_text` | 此時無法從檔案總管拖放檔案，也無法使用右鍵選單。
請以一般使用者身分重新啟動。 |
| `home.elevated_restart` | 以一般使用者身分重新啟動 |
| `home.elevated_restart_failed` | Filee 無法自行重新啟動。
請從系統匣結束 Filee，然後再次啟動。 |
| `format_request.notice_title` | Filee 暫時無法轉換 {0} 檔案 |
| `format_request.notice_text` | 告訴我們你需要的格式。
我們會根據請求決定接下來支援哪些格式。
加密或有複製保護 (DRM) 的檔案，無論什麼格式都無法轉換。 |
| `format_request.request` | 請求支援此格式 |
| `common.close` | 關閉 |

#### C. 系統匣

`tray.*` 出處 `TrayService.cs:74-80`；`triggers.paused` 為暫停時的 tooltip；`general.close.*`、`close.*` 只與設定視窗的關閉行為有關（Rust 常駐程式可不實作）。

| key | zh-TW |
|---|---|
| `tray.open` | 開啟 Filee |
| `tray.pause` | 暫停快捷操作 |
| `tray.resume` | 恢復快捷操作 |
| `tray.quit` | 結束 |
| `tray.update` | 更新到 {0} |
| `tray.tooltip` | Filee：按住 {0} 拖曳檔案 |
| `triggers.paused` | 快捷操作已暫停 |
| `general.close_action` | 關閉視窗時 |
| `general.close.Ask` | 每次詢問 |
| `general.close.Tray` | 在系統匣中繼續執行 |
| `general.close.Quit` | 結束 Filee |
| `close.title` | 讓 Filee 繼續執行嗎？ |
| `close.text` | 保留在系統匣時，環形工具列和自動轉換資料夾會繼續運作。
之後可以在 設定 → 一般 中變更。 |
| `close.tray` | 保留在系統匣 |
| `close.quit` | 結束 |
| `close.dont_ask` | 不再詢問 |

#### D. donut 中央（hub）文字、提示與停用原因

donut 的文字來源 `A/ViewModels/RadialViewModel.cs:136,179-196,215,264-292`。hub 兩行：`CenterTitle`（大字）、`CenterSubtitle`（小字）。狀態對照見表後。

| key | zh-TW |
|---|---|
| `donut.center_drag` | 把檔案拖到這裡 |
| `donut.files_same` | {0} 個 {1} |
| `donut.files_mixed` | {0} 個檔案 |
| `donut.drop_to` | 放開 → {0} |
| `donut.pick_in_group` | 向外移動以選擇 |
| `donut.group_empty` | 此群組中已沒有預設，請在「設定 → 環形工具列」中編輯 |
| `donut.click_to` | 點擊 → {0} |
| `donut.cancel` | 拖到這裡取消 |
| `donut.close_hint` | 按 Esc 關閉 |
| `donut.edit` | 編輯預設 |
| `donut.empty` | 把預設拖到這裡 |
| `donut.same_format` | 同格式 |
| `donut.group_caption` | 群組 · {0} 個項目 |
| `donut.flip_to_back` | {0} 背面 |
| `donut.flip_to_front` | {0} 正面 |
| `engine.reason.not_installed` | 未安裝 |
| `engine.reason.error` | 檢查時出錯 |
| `engine.reason.unsupported_platform` | 此平台不可用 |
| `engine.reason.needs_package` | 需要 {0}，請在「設定 → 轉換引擎」中安裝 |

#### E. 失敗原因 `error.*`（toast 紅字、donut 停用原因都用）

全部 53 個；`ErrorKey` 缺省時用 `error.conversion_failed`。其中 **donut 切片停用原因**用到：`error.unsupported_source`、`error.no_route`、`error.images_only`、`error.no_transparency`、`error.cannot_turn`、`engine.reason.needs_package`（{0}=需要安裝的引擎名）、`donut.group_empty`（出處 `A/Services/PresetAvailability.cs:113-246`、`RadialViewModel.cs:267`）。

| key | zh-TW |
|---|---|
| `error.unsupported_source` | 不支援此檔案類型 |
| `error.empty_file` | 檔案是空的（0 位元組）。用右鍵選單「新增」建立的檔案，要在對應的應用程式中開啟並儲存後才有內容 |
| `error.no_route` | 沒有引擎可以完成此轉換 |
| `error.msi_busy` | 另一個安裝正在進行。請在它完成後再試一次 |
| `error.msi_link` | Windows Installer 無法開啟安裝套件：路徑中的某個資料夾可能是指向其他磁碟機的連結 |
| `error.cannot_turn` | 只能旋轉和翻轉圖片 |
| `error.images_only` | 只能用於圖片 |
| `error.no_transparency` | 此格式沒有透明部分，無需裁切 |
| `error.ocr_language_download` | 無法下載 OCR 語言資料 |
| `error.ocr_needs_tesseract` | Apple Vision 無法辨識此語言。請在「設定 → 轉換引擎」中安裝 Tesseract |
| `error.too_many_frames` | 影格數太多，請調大「每 N 個影格」 |
| `error.too_many_pictures` | 圖片太多，無法放進同一部影片，請分成幾次 |
| `error.merged_image_too_large` | 合併後的圖片太大，請減少圖片數量或使用較小的圖片 |
| `error.over_size_limit` | 無法縮小到大小上限以內 |
| `error.not_enough_space` | 磁碟空間不足，無法儲存這些圖片 |
| `error.no_motion_photo` | 這張照片中沒有可讀取的影片（Android 動態相片，或旁邊有 MOV 的 iPhone 原況照片） |
| `error.live_photo_start` | 片段開始位置在影片結束之後。請把「片段開始」設得更早 |
| `error.live_photo_hdr` | 此 FFmpeg 沒有 zscale 濾鏡，無法把 HDR 影片製作成原況/動態相片。Filee 下載的 FFmpeg 帶有該濾鏡 |
| `error.rhwp_timeout` | 文件太長，rhwp 未能在時限內完成 |
| `error.rhwp_timeout_calibre` | 文件太長，rhwp 未能在時限內完成。如果是電子書，請在「設定 → 轉換引擎」中把 Calibre 移到 HWPX writer 之上，Calibre 產生 PDF 要快得多 |
| `error.engine_stalled` | 轉換引擎停止回應：長時間沒有進展，已將其停止 |
| `error.needs_shaping` | 這種文字需要 LibreOffice 才能正確轉換，請在「設定 → 轉換引擎」中安裝 |
| `error.runtime_missing` | 此引擎所需的 Microsoft Visual C++ 執行階段程式庫檔案缺失或版本過舊。請更新或重新安裝 Filee，或安裝 Microsoft Visual C++ 可轉散發套件 (x64) |
| `error.conversion_failed` | 轉換引擎出錯 |
| `error.pdf_no_tables` | 此 PDF 沒有以線條繪製的表格，因此沒有可放入試算表的內容。掃描的 PDF 完全沒有文字。 |
| `error.pdf_password_gone` | 歷史記錄不保存預設的 PDF 密碼，而該預設已沒有密碼。請用有密碼的預設重新轉換這些檔案。 |
| `error.web_download_failed` | 無法下載該檔案 |
| `error.web_page` | 該連結開啟的是網頁，不是檔案。需要登入才能獲取的檔案 Filee 無法下載，請先在瀏覽器中儲存 |
| `error.web_too_large` | 檔案大於 2 GB |
| `error.web_blocked` | 不儲存程式、安裝檔和指令碼 |
| `error.web_timeout` | 伺服器沒有回應 |
| `error.geo_empty` | 檔案中沒有地理特徵 |
| `error.geo_unknown_crs` | 座標不是經緯度，檔案也沒有說明座標系統（Shapefile 請把 .prj 檔案放在旁邊） |
| `error.geo_unsupported_crs` | 暫不支援此座標系統 |
| `error.geo_bad_coordinates` | 座標超出了該座標系統的範圍 |
| `error.geo_csv_no_coordinates` | CSV 中沒有經緯度欄，也沒有 WKT 欄 |
| `error.geo_csv_no_locations` | CSV 中沒有任何一列能讀出位置 |
| `error.geo_unsupported_shape` | 不支援此類形狀 |
| `error.geo_too_large` | 將超過 Shapefile 2 GB 的上限 |
| `error.fit_not_fit` | 這不是 Garmin FIT 檔案 |
| `error.fit_damaged` | FIT 檔案已損毀或不完整 |
| `error.fit_chained` | 暫不支援多個 FIT 檔案首尾相連的情況 |
| `error.fit_too_large` | FIT 檔案太大，無法轉換 |
| `error.fit_no_file_id` | FIT 檔案未說明檔案類型 |
| `error.fit_not_activity` | 此 FIT 檔案不是活動記錄（只能轉換活動記錄） |
| `error.fit_no_positions` | 活動沒有記錄位置資訊，請轉換為 CSV、XLSX 或 JSON |
| `error.fit_no_records` | 活動沒有記錄資料 |
| `error.fon_vector` | 這是向量字型，只能轉換點陣 .fon 字型 |
| `error.fon_unsupported` | 不支援此類 .fon 字型 |
| `error.fon_too_large` | .fon 字型太大，無法轉換 |
| `error.fon_damaged` | .fon 檔案已損壞或不完整 |
| `error.m4s_needs_init` | 此 .m4s 是串流影片的一個片段，沒有另外的起始（init）區段就無法讀取 |
| `error.cannot_hide` | 只能對圖片加上馬賽克或模糊 |

#### F. 完成備註 `notice.*`（toast 灰字，每檔一行）

加括號原因的 `trash.cannot_place` 為 `notice.original_in_trash` 的 `NoticeReasonKey`（`JobQueue.cs:1199-1200`）；`notice.original_kept` 的原因 key 來自 `why.ReasonKey`（`JobQueue.cs:1217-1218`，具體 key 集合未列：搜尋範圍僅 `Filee.Core/Conversion/JobQueue.cs`）。

| key | zh-TW |
|---|---|
| `notice.pdf_not_smaller` | 無法再縮小，已原樣複製 |
| `notice.cad_newer_left_out` | 已略過所選 AutoCAD 版本沒有的物件（複線引線、網格……） |
| `notice.original_kept` | 已保留原始檔案，結果另存為副本 |
| `notice.original_in_trash` | 原始檔案已在資源回收筒/垃圾桶中，結果保留了自己的名稱 |
| `notice.original_unchanged` | 無法變小，因此保留原始檔案不變 |
| `notice.empty_spreadsheet_pages_removed` | 已移除試算表中的空白頁面 |
| `notice.ocr_has_text` | 每頁都已有文字，已原樣複製 |
| `notice.live_photo_pair` | 已儲存為同名的照片和 MOV。將兩者一起匯入（iCloud 照片、隔空投送或 Mac 上的「照片」）即可得到原況照片 |
| `notice.geo_datum_assumed` | 基準面不是 WGS 84，已按相同處理，位置可能偏差數百公尺 |
| `notice.geo_gpx_lossy` | GPX 只能儲存點、路線和軌跡及少數字段，其他屬性已省略，面變成了軌跡 |
| `notice.geo_text_cut` | 部分文字超過 Shapefile 欄位長度（254 位元組），已被截斷 |
| `notice.geo_columns_dropped` | Shapefile 最多 255 欄，多出的欄已省略 |
| `notice.geo_rows_without_location` | 部分列無法讀出位置，已不帶形狀儲存 |
| `notice.geo_field_names` | Shapefile 欄位名最多 10 個字元，部分名稱已縮短 |
| `notice.geo_shapefile_split` | Shapefile 只能儲存一種形狀，因此點、線、面被分別存成了多個檔案 |
| `notice.geo_heights_dropped` | 部分點沒有高度，而 Shapefile 無法只省略其中一個點的高度，因此已省略所有高度 |
| `trash.cannot_place` | 無法將結果儲存到原始檔案所在的資料夾 |

#### G. 主題頁／外觀名稱（Rust 常駐程式用不到，僅供參考）

| key | zh-TW |
|---|---|
| `theme.style.Soft` | Filee Soft |
| `theme.style.MaterialYou` | Material You |
| `theme.mode.System` | 跟隨系統 |
| `theme.mode.Light` | 淺色 |
| `theme.mode.Dark` | 深色 |


#### D'. donut hub 狀態對照（供 donut 規格交叉引用）

| 情況 | 大字 `CenterTitle` | 小字 `CenterSubtitle` | 出處 |
|---|---|---|---|
| 拖曳模式，還沒有檔案進來 | `donut.center_drag`「把檔案拖到這裡」 | 無 | `RadialViewModel.cs:136-137` |
| 已有檔案，全部同一格式 | `donut.files_same`「{0} 個 {1}」，{0}=檔案數（Shapefile 的 .dbf/.shx 等伴隨檔不計）、{1}=格式顯示名，例「3 個 PNG」 | 點擊模式：`donut.close_hint`「按 Esc 關閉」；拖曳模式：無 | `:156,179-182` |
| 已有檔案，格式混合／未知 | `donut.files_mixed`「{0} 個檔案」 | 同上 | `:179-182` |
| 滑過可用切片 | （不變） | 拖曳模式：`donut.drop_to`「放開 → {名稱}」；點擊模式：`donut.click_to`「點擊 → {名稱}」。名稱 = 切片 `Label`（preset 的 `DonutName`）；群組子環上 = `{群組名} · {子名}` | `:287-289,282-284` |
| 滑過被停用切片 | （不變） | 該切片的 `DisabledReason`（§5.2-E 的 `error.*`、`engine.reason.needs_package`、`donut.group_empty`） | `:290,285` |
| 滑過群組切片（未進子環） | （不變） | `donut.pick_in_group`「向外移動以選擇」 | `:286` |
| 滑過中心 | （不變） | `donut.cancel`「拖到這裡取消」 | `:278-279` |
| 沒有滑過任何東西 | （不變） | 點擊模式：`donut.close_hint`；否則無 | `:291-292` |
| 可翻面時的角落提示 `FlipHint` | `donut.flip_to_back`「{0} 背面」／`donut.flip_to_front`「{0} 正面」，{0}=翻面鍵的顯示名（若只用滾輪翻則為空，字串 `Trim()`） | | `:213-217` |
| 切片副標（`Caption`） | 群組：`donut.group_caption`「群組 · {0} 個項目」；「與來源相同」格式：來源格式名最多 2 個以 `/` 連接；其他：目標格式名（`FormatLabels.NameOf`） | | `:245-248,264` |

`donut.same_format`「同格式」、`donut.edit`、`donut.empty` 在設定視窗（工具列頁、群組編輯器）使用，不是 donut 浮層（`ToolbarPageViewModel.cs:874`、`GroupEditorViewModel.cs:157`；`donut.edit`、`donut.empty` 的使用處我沒有追到，搜尋 `src/**/*.cs|*.axaml` 找不到 `donut.edit` 與 `donut.empty` 的引用，可能是 XAML 以 `DynamicResource` 取用但 grep 未命中，**未確認**）。

---

## 6. 出處索引（本檔引用過的上游檔案與行號段）

| 主題 | 檔案 : 行 |
|---|---|
| Toast 視窗／版面／動畫 | `A/Views/ToastWindow.axaml:1-137`；`ToastWindow.axaml.cs:14,20,25-37,60-76,78-88` |
| Toast 狀態與文字 | `A/ViewModels/JobViewModel.cs:14-165`（尤其 30-31、104-146、148-164）；`A/ViewModels/FailureText.cs:13-14`；`C/Localization/ILocalizer.cs:32-38` |
| Toast 清單／計時／重試 | `A/Services/ConversionService.cs:20-24,69-92,105-123,166-172,200-203,213-250`；`A/Services/RadialController.cs:49,205-213,457-511` |
| 下載徽章 | `A/ViewModels/DownloadBadge.cs:15-63`；`A/Services/EngineDownloadService.cs:151-157` |
| 字型連結 | `A/Services/FontSources.cs:9-40` |
| 格式請求 | `A/Services/FormatRequest.cs:11-36` |
| 減少動態 | `A/Services/Motion.cs:1-77`；`Filee.Platform.Windows/WindowsPlatformServices.cs:247-248` |
| 角落通知 | `A/Views/{UpdateNotice,EngineUpdateNotice,FeedbackNotice,ElevationNotice,FormatRequestNotice}Window.axaml(.cs)`；`A/Services/WindowService.cs:493-643`；`A/Services/UpdateTexts.cs:7-50`；`A/App.axaml.cs:148-150,201-202,226-259` |
| 系統匣 | `A/Services/TrayService.cs:1-84`；`A/Services/GestureText.cs:9-17`；`A/Views/MainWindow.axaml.cs:38-93`；`C/Settings/AppSettings.cs:22-33,140-142,167,182` |
| 主題套用 | `A/Services/ThemeService.cs:23-24,35-45,57-68,70-164,169-179`；`A/Services/SoftPalette.cs:15-18,21-38,66-116,122-146,149-191`；`A/Services/MaterialYouPalette.cs:61-247` |
| 色票 / 樣式 | `A/Styles/Soft.axaml:14-62,66-69,83-98,111-132,156-199,434-449,560-566`；`A/App.axaml:25-42` |
| 自訂主題 | `C/Settings/CustomTheme.cs:12-99`；`D/CUSTOM-THEMES.md:1-79`；`D/themes/*.json`、`D/themes/NOTICE.md` |
| 設定格式 | `C/Settings/AppSettings.cs:9-58,131`；`C/Settings/FileeJsonContext.cs:11-19` |
| 多語系 | `A/Services/LocalizationService.cs:25-157`；`A/Services/AppLanguages.cs:9-62`；`A/Assets/i18n/*.json`；`D/ADDING-A-LANGUAGE.md`；`D/ARCHITECTURE.md:237-260` |
| 圖示／字型授權 | `A/Assets/Icons/{filee.ico,filee.png,filee.svg}`；`A/Styles/Icons.axaml`；`A/Program.cs:305-309`；`upstream/THIRD-PARTY-NOTICES.md:104-114`；`upstream/LICENSE` |

---

## 7. 移植建議（Rust 端）

### 7.1 繪製方式

**toast、五種通知卡、（選用）自繪選單：每一張都是「分層視窗」**——`WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`，`WS_POPUP`，不要 `WS_EX_APPWINDOW`；`WM_MOUSEACTIVATE` 回 `MA_NOACTIVATE`；顯示用 `SW_SHOWNA`／`SWP_NOACTIVATE`（對應上游 `ShowActivated=False`）。以**每像素 alpha** 畫透明圓角卡片＋陰影。

建議兩條路，擇一：

1. **Direct2D＋DirectWrite（建議）**：`ID2D1DCRenderTarget`（BGRA、`D2D1_ALPHA_MODE_PREMULTIPLIED`）畫到 32-bit DIB，再 `UpdateLayeredWindow(ULW_ALPHA)`。優點：DirectWrite 有現成的**字型 fallback 與繁中字形選擇**（`IDWriteFontFallback`／`CreateFontFallback`，或直接指定 §4.7 的家族鏈）、ClearType 之外的灰階抗鋸齒適合透明視窗、圓角矩形／陰影（可用兩層 `FillRoundedRectangle` 疊模糊近似，或 D2D Effect 的 Shadow／GaussianBlur，需 `ID2D1DeviceContext`）。依賴 `windows` crate。
2. **tiny-skia ＋ 自選文字引擎**：tiny-skia 沒有文字，需另配 `cosmic-text`／`rustybuzz`＋`fontdue`/`swash`。CJK fallback、複雜文字（阿拉伯、印地、泰）要自己處理；繁中使用者影響小，但上游支援 17 種語言。優點：不依賴 D2D，易做跨平台截圖測試。

共通做法：
- **DPI**：per-monitor v2 感知；取主螢幕工作區（`SystemParametersInfo(SPI_GETWORKAREA)` 或 `MonitorFromPoint(PRIMARY)`＋`GetMonitorInfo.rcWork`），尺寸 = DIP × `GetDpiForMonitor/96`。上游「螢幕邊 12」是實體 px 而內側透明邊有乘縮放——要逐像素一致就照抄，否則統一乘縮放（視覺差 ≤ 6 px @150%）。
- **陰影**：視窗要預留 12 DIP 透明邊容納；陰影兩層（§4.3 `CardShadow`）。透明邊也是點擊穿透區的一部分——用 `WM_NCHITTEST` 在透明邊回 `HTTRANSPARENT`。
- **動畫**：用單一 ~16 ms 計時器（`SetTimer` 或 `DwmFlush` 迴圈）只在有動畫時跑；easing 以標準 easeOutBack（`c1=1.70158`：`t'=t-1; 1 + t'^2((c1+1)t' + c1)`）；進場 = 位移 40 DIP＋opacity，0.35 s；徽章 = scale 0.6→1＋opacity，0.22 s；reduced-motion 時關掉（SPI_GETCLIENTAREAANIMATION 或 `theme.reduceMotion`）。
- **互動**：自己做 hit-test（按鈕、X）。hover／pressed 顏色用 §4.3；按下 scale 0.95 可省略。游標改手形。
- **多張卡片**：上游每個 job 是同一視窗內的一列；Rust 可以單一分層視窗＋自己排版（最像上游：整個視窗貼右下錨定、向上長），或每卡一窗。**建議單一視窗＋統一管理角落通知的 `BottomOffset`**，順手修掉上游「回饋／管理員／格式請求會蓋住 toast」的缺口。
- **熱路徑**：進度更新（每次最多幾十次／秒）要合併重繪（上游也在 `OnJobUpdated` 合併；`ConversionService.cs:206-211`）。
- **透明視窗被第三方工具染色**（Windows 的 accent blur／backdrop，issue #269）：上游偵測並清除（`A/Services/TransparentWindows.cs`）；Rust 自繪的 `UpdateLayeredWindow` 視窗不吃 DWM backdrop，這個問題理論上不存在〔中〕。

### 7.2 系統匣

- `Shell_NotifyIconW`（`NOTIFYICON_VERSION_4`，`uCallbackMessage`），隱藏視窗收訊息。圖示：從 `filee.ico` 取 16/24/32 px（`LoadImageW(LR_LOADFROMFILE)` 或內嵌資源＋`LoadIconMetric(LIM_SMALL)`，讓高 DPI 取到 32/48）。**處理 `TaskbarCreated` 廣播重新加入圖示**（Explorer 重啟）。
- 左鍵（`NIN_SELECT`／`WM_LBUTTONUP`）→ 啟動設定視窗（Filee.exe `--settings`，`CommandLine.cs:112`；是否已有實例的處理屬另一份規格，注意：直接啟動 Filee.exe 會同時啟動它**自己的**匣圖示與觸發器 → 要確認上游是否有「僅設定視窗」模式；`--hosted` 的語意我沒有讀，**未確認**，見 §8）。
- 右鍵（`WM_CONTEXTMENU`／`NIN_CONTEXTMENU`）→ 彈選單。兩種做法：**(a)** 原生 `CreatePopupMenu`＋`TrackPopupMenu`（先 `SetForegroundWindow`，彈完 `PostMessage(WM_NULL)`；標準流程）。簡單、可靠，但外觀是 Win32 選單，不像上游的 Fluent 風；Windows 11 下原生選單是圓角且會跟系統深淺色，用 `SetPreferredAppMode`（uxtheme 序數 135，未公開 API）才能在 Win10 跟深色；**(b)** 以 §7.1 自繪一張小選單窗（圖面最像上游，但鍵盤操作、無障礙、失焦關閉要自己做）。**建議 (a)**，4 個項目不值得自繪。
- tooltip：`NOTIFYICONDATA.szTip`（最多 127 字元＋結尾）：暫停 → `triggers.paused`；否則 `tray.tooltip`＋手勢（§3.1）。
- 防毒（Bitdefender）注意：這是一個常駐、會註冊匣圖示與全域 hook 的未簽章 exe，行為組合容易被擋——匣圖示本身沒問題；**不要**在程式內自行寫 Run／Startup 項目，交給使用者或安裝程式。

### 7.3 主題與設定

- 啟動與**每次 settings.json／theme.json 變更**重新讀取（上游是設定存檔事件；Rust 沒有那個事件，需用 `ReadDirectoryChangesW`／`notify` 監看 `%APPDATA%\Filee\`，並去抖 100–300 ms，因為設定頁的圓角滑桿每 tick 都存檔）。
- 實作順序建議：色票（§4.3）→ 衍生 accent（§4.4）→ cornerRadius（§4.5）→ theme.json（§4.8）→ preset（§4.4）→ **MaterialYou（§4.6）可後做**，並可先降級成「Soft」。
- 資料結構：`Palette { page, surface, surface_alt, subtle_border, text, text_secondary, danger, success, warning, card_shadow[2], accent, accent_soft, accent_faint, accent_strong, on_accent }` × Light／Dark 兩份。
- 圖示（`Icon.Close/Folder/Refresh/Download/Engine`）是 Material Design Icons 的 24×24 SVG path，**路徑資料**在 `A/Styles/Icons.axaml`，可直接當 SVG `d` 使用：
  - `Icon.Close`：`M19,6.41L17.59,5L12,10.59L6.41,5L5,6.41L10.59,12L5,17.59L6.41,19L12,13.41L17.59,19L19,17.59L13.41,12L19,6.41Z`（`Icons.axaml:21`）
  - `Icon.Folder`：`M10,4H4C2.89,4 2,4.89 2,6V18A2,2 0 0,0 4,20H20A2,2 0 0,0 22,18V8C22,6.89 21.1,6 20,6H12L10,4Z`（`:15`）
  - `Icon.Refresh`：`M17.65,6.35C16.2,4.9 14.21,4 12,4A8,8 0 0,0 4,12A8,8 0 0,0 12,20C15.73,20 18.84,17.45 19.73,14H17.65C16.83,16.33 14.61,18 12,18A6,6 0 0,1 6,12A6,6 0 0,1 12,6C13.66,6 15.14,6.69 16.22,7.78L13,11H20V4L17.65,6.35Z`（`:35`）
  - `Icon.Download`：`M5,20H19V18H5M19,9H15V3H9V9H5L12,16L19,9Z`（`:28`）
  - `Icon.Engine`：`M17,17H7V7H17M21,11V9H19V7C19,5.89 18.1,5 17,5H15V3H13V5H11V3H9V5H7C5.89,5 5,5.89 5,7V9H3V11H5V13H3V15H5V17A2,2 0 0,0 7,19H9V21H11V19H13V21H15V19H17A2,2 0 0,0 19,17V15H21V13H19V11M13,13H11V11H13M15,9H9V15H15V9Z`（`:13`）
  - `Icon.Donut`（`Icons.axaml:10`，含 `F1` fill-rule 前綴）等其餘圖示見該檔。
  PathIcon 的顏色 = 該處 `Foreground`（Close/Folder/Refresh = 文字色；accent 按鈕上的圖示 = 按鈕前景；`Icon.Engine` = `AccentStrong`）。注意 `F1 M…` 開頭的是 fill-rule 指令，需轉 `fill-rule="nonzero"`（上游 `Icon.Donut`）。
- 字型：見 §4.7。三種策略：**(A) 零資產**：直接用 `SystemFontFamilies` 鏈（Segoe UI＋Microsoft JhengHei UI），最省事，與上游差在 Latin 字形（上游優先 Noto Sans）；**(B) 附 Noto Sans（Regular/SemiBold 靜態檔）**＋系統 CJK，Latin 字形與上游一致；**(C) 全照抄**（附 7 個 Noto 檔，約數十 MB）。建議 **(B)**；Noto 檔自行由 `https://github.com/notofonts` 取得（上游 repo 不含）。

### 7.4 可直接沿用的上游資產與授權

| 資產 | 路徑 | 授權／注意 |
|---|---|---|
| **i18n 字串** 17 語言 | `A/Assets/i18n/*.json` | MIT（Copyright (c) 2026 Myeonggu Kim (KnifeLemon) and Filee contributors）。可整檔沿用；要保留 MIT 著作權聲明與授權文字。格式為單層 JSON、`{0}` 佔位符，Rust 端自寫 `format` 即可（`{n}` 取代；不需 .NET 完整 format，上游文字只用到純 `{n}`，已用腳本檢查 zh-TW.json：沒有 `{0:…}` 這類格式規格，只有純 `{n}`） |
| **應用程式圖示** | `A/Assets/Icons/filee.ico`（6 尺寸）、`filee.png`（512²）、`filee.svg` | 與程式碼同在 MIT 倉庫內，倉庫未另列商標限制（`THIRD-PARTY-NOTICES.md` 只對贊助平台 logo 標註商標；搜尋 `README.md`、`THIRD-PARTY-NOTICES.md` 無 Filee 圖示商標條款）；若要以「相容常駐程式」名義散布，建議保留 Filee 名稱與出處聲明 〔中〕 |
| **Material Design Icons 路徑** | `A/Styles/Icons.axaml` | Apache-2.0（Pictogrammers）；需保留授權聲明（`THIRD-PARTY-NOTICES.md:110`） |
| **自訂主題範例** | `D/themes/*.json` | 各自 MIT（見 `D/themes/NOTICE.md`） |
| **色票／公式** | 本檔 §4 | 屬程式碼的一部分，MIT，需保留著作權聲明（在 Rust repo 的 NOTICE 中註明） |
| **Noto 字型** | **不在上游 repo** | SIL OFL 1.1；可自由內嵌與再散布，但字型本身不可單獨販售、須附 OFL 全文與著作權聲明 |
| **Simple Icons（贊助 logo）** | `A/Styles/Brands.axaml` | CC0；商標屬各平台。Rust 常駐程式用不到 |

### 7.5 取捨建議（範圍壓縮）

- **必做**：toast（含取消、檢視、重試、錯誤明細、備註、缺字型、請求格式、下載徽章）、系統匣（4 個項目）、主題 Soft＋theme.json＋accent 衍生、i18n 載入與 `auto`。
- **可後做**：MaterialYou（§4.6）、`presetKey`、更新通知與引擎更新通知（若更新仍由 Filee.exe 設定程式自己處理就不需要；Rust 常駐程式若不做更新檢查，連 `update.*` 通知都可省）、回饋卡（純粹行銷，使用者可能不要）、RTL 鏡射。
- **建議保留**：系統管理員通知（UIPI 實際會讓拖放失效）、格式請求通知（低成本）。
- **新架構才有的新卡片**（上游沒有，Rust 版自己要設計）：「找不到／無法啟動 `filee-cli.exe`」、「`filee-cli` 逾時／異常結束」、「Filee.exe 設定程式啟動失敗」。可借用現有卡片版面與 `error.conversion_failed`。

---

## 8. 未找到／未確認／跨規格問題

**未找到（附搜尋範圍）**
1. 系統匣的氣泡通知、依狀態切換的匣圖示：整個 `upstream/src` 搜尋 `Balloon`、`ShowNotification`、`ToastNotification`、`Icon =`、`WindowIcon`——無。
2. toast 的 hover 暫停計時、最大高度、離場動畫、依 `Paused` 隱藏：搜尋 `Views/ToastWindow.axaml*`、`Services/ConversionService.cs`——無。
3. `TrayPopupRoot`（Avalonia 內部）原始碼：搜尋全 `src`——無（Avalonia 套件內）。
4. Noto 字型檔：搜尋 `upstream/**/*.ttf|*.otf` 與 `Filee.dll` 內嵌資源——無。
5. Fluent 內建 accent 按鈕／ProgressBar 的確切顏色：Avalonia 套件內，上游 repo 無。已以實機截圖 `Filee 轉換引擎安裝.png`（本專案資料夾，Light 模式）確認 accent 按鈕為 accent 色底＋白字，其餘細節未驗證。

**未確認（可信度低，實作時請實機比對）**
- Windows 上 `mode=System` 的實際讀取方式（Avalonia 內部）、`auto` 取 OS 語言的 API。
- 全部「Fluent 預設」的控制項細節：按鈕高度（我估 ≈30 DIP：行高 ≈19 + 上下 Padding 8 + 邊框 2）、進度條軌道色與填色（填色可能是 `SystemAccentColor`，與 `AccentBrush` 同色，或 Light 模式下的 `Dark1`）、accent 按鈕 hover/pressed 色、Close 按鈕 hover 底色。
- `Skipped` 檔是否寫入 `Outputs`（影響「已跳過」狀態是否出現「檢視」按鈕）。
- 動畫 `BackEaseOut` 的 overshoot 常數（採標準值 1.70158）。
- 左鍵雙擊匣圖示的行為（上游只掛 `Clicked`）。

**跨規格問題（請主 agent 轉給相關規格）**
1. **進度來源**：Toast 的進度百分比、逐檔狀態、`ErrorKey`／`NoticeKey`／`MissingFonts` 在上游是 in-process 物件（`ConversionJob`）；Rust 呼叫 `filee-cli.exe` 時只拿得到 CLI 的輸出（`ok/skip/stop/fail` 行與 `--json` 的結果，沒有逐檔百分比、沒有錯誤 key、`missing fonts` 只有一段括號文字；`Filee.Cli/Cli.cs:100-215`）。Toast 需要的資料（進度、狀態、錯誤原因 key、缺字型清單、unsupported_source 判斷）要請 CLI 規格確認是否可輸出，或由 Rust 自行：用檔案數推進度、用 `FormatRegistry` 規則判斷「未知副檔名」。
2. **donut 進度環與下載徽章**：donut 版的徽章位置（中央上方 70 DIP、12px 字、Padding 10,4）與動畫（Opacity 0.16 s＋scale 0.6→1 0.22 s）在 `A/Views/RadialWindow.axaml:23-39,57-64`，由 donut 規格處理；文字產生邏輯與本檔 §1.4 共用。
3. **Filee.exe 的「只開設定視窗」模式**：匣的「開啟 Filee」與 toast 無關但需要；`--settings`、`--hosted` 的語意由啟動／IPC 規格確認，避免啟動 Filee.exe 時它也開自己的匣圖示與觸發器造成雙重常駐。
4. **設定寫入競爭**：`paused` 由匣選單改寫 settings.json；Filee.exe 設定頁也寫同一檔。需要檔案鎖／原子寫入約定（上游單一行程寫入，沒有這個問題）。

**做完後值得另外做的（不在本次範圍）**
- 把本檔 §4.3／§4.4／§4.6 的色票與公式整理成一份機器可讀的 `palette.json`＋對照測試（以 theme.json 範例與上游預設值驗證衍生公式），避免 Rust 實作與上游漂移。
- 針對 §8「未確認」項目，在本機裝上游 Filee 2.2.0 後以截圖量測（按鈕高度、進度條、字型實際渲染）補上實測值。
