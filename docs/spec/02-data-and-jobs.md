# 02 資料格式與轉檔流程（Rust 常駐外殼 × 上游 Filee v2.2.0）

範圍：四個 JSON 檔、history 寫法、ConversionService 與 `filee-cli convert` 行為、自動轉換資料夾、SingleInstance／命令列／登錄檔、移植風險。
出處縮寫：`src/` 指 `upstream/src/`；`Core/…` = `src/Filee.Core/…`、`App/…` = `src/Filee.App/…`、`Cli/…` = `src/Filee.Cli/…`、`WinPlat/…` = `src/Filee.Platform.Windows/…`。行號是 v2.2.0 原始碼的實際行號。
標記：**〔實測〕** = 我在這台機器跑過；**〔讀碼〕** = 只讀原始碼；**〔未實測〕** = 推論，沒驗證。

---

## 0. 結論先看（給實作者的 10 條）

1. 四個檔都放在 `%APPDATA%\Filee\`（`Core/Settings/UserDataStore.cs:42-43`），環境變數 `FILEE_DATA_DIR` 可改位置（同檔 :33）。
2. 檔案格式：UTF-8 **無 BOM**、2 空格縮排、camelCase 屬性名、enum 一律字串（PascalCase 名稱）、`null` 欄位整個省略（`Core/Settings/FileeJsonContext.cs:10-16`）。上游寫出的換行是 Windows `\r\n`〔實測：settings.json 是 CRLF；本機 presets.json 卻是 LF，讀取端兩種都吃〕。
3. **讀取是嚴格的**：屬性名大小寫要對（source-generated 預設區分大小寫）；非 lenient 的 enum 值錯 → 整個檔被視為損毀 → 複製成 `*.broken`、整份改用內建預設，之後上游一存檔就把你的資料蓋掉（`UserDataStore.cs:304-324`）。**Rust 寫檔前必須用 round-trip（`serde_json::Value` 保序）只改自己負責的欄位，其餘原樣保留。**
4. 上游 App（Filee.exe）**只在啟動時讀一次檔，之後不再重讀、也沒有 FileSystemWatcher**（`App/App.axaml.cs:66` 唯一的 `store.Load()`；全專案沒有重讀）。CLI（filee-cli.exe）每個程序啟動都重讀，所以 Rust 改完檔，下一個 CLI 呼叫立刻生效。
5. `filee-cli convert --json` 只在**全部結束時輸出一次 JSON**，沒有進度。人類可讀模式（不加 `--json`）會在每個檔案完成時印一行 `ok/skip/stop/fail`——這是唯一的準進度來源（粒度＝檔案）。〔實測〕
6. **CLI 完全不寫 history.json**（全專案只有 `ConversionService.OnJobFinished` 與三個互動工具呼叫 `AddHistory`）。Rust 要自己寫（見 §2）。
7. CLI 不支援的 App 端功能：瀏覽器拖曳下載、「每次詢問存檔位置」、互動工具（Crop/Clip/Image edit/Hide/PDF 工具）、Undo、Retry、Toast；CLI 支援 Replace original（資源回收筒）與 MissingFonts（只回報名稱）。
8. `filee watch` **不適合直接取代**：每個 rule 一個 ~45–65 MB 的程序、不支援 `include`／`fileNamePattern`／`renames`、不寫 history、rule id 固定 `"cli"`。建議 Rust 自己實作 FolderWatcher，批次丟給 `filee-cli convert`（見 §4）。
9. 單一執行個體：互斥體 `Local\Filee-B512D97E7CBF`、具名管道 `\\.\pipe\Filee-B512D97E7CBF`（本機 UserName=`User`，管道實測存在）；協定＝一個 JSON 字串陣列（Filee.exe 的完整 argv）。Rust 要當 first instance 就必須自己持有這兩個名稱（§5）。
10. Win11 右鍵選單的原生 DLL **寫死啟動 `Filee.exe`**（`src/Filee.ExplorerMenu/FileeExplorerMenu.c:32`），所以 Filee.exe 必須繼續安裝，它在 Rust 持有管道時只會「轉發 argv 後立刻結束」。

---

## 1. 四個 JSON 檔

### 1.1 共通寫入機制

| 項目 | 上游行為 | 出處 |
|---|---|---|
| 序列化選項 | `WriteIndented=true`、`PropertyNamingPolicy=CamelCase`、`UseStringEnumConverter=true`、`DefaultIgnoreCondition=WhenWritingNull`、讀取時 `ReadCommentHandling=Skip`、`AllowTrailingCommas=true` | `Core/Settings/FileeJsonContext.cs:10-16` |
| 編碼 | `File.Create` + `JsonSerializer.Serialize(Stream)` → UTF-8 **無 BOM**〔實測 `od`：settings.json 開頭 `{ \r \n`；history.json 開頭 `[ ]`〕 | `UserDataStore.cs:326-337` |
| 縮排／換行 | 2 空格、`Environment.NewLine`（Windows = CRLF）〔實測 settings.json=CRLF、presets.json=LF〕 | 同上 |
| atomic write | 寫 `<path>.tmp` → `File.Move(temp, path, overwrite:true)`；整段包在進程內 `lock(_saveLock)`（**沒有跨進程鎖**） | `UserDataStore.cs:326-337` |
| 讀取失敗 | 例外為 `JsonException`／`IOException`／`NotSupportedException` 時：`File.Copy(path, path + ".broken", overwrite:true)`，回傳 null → 該檔改用預設值（settings→`new AppSettings()`、presets/profiles→`BuiltInData`、history→空） | `UserDataStore.cs:304-324`，呼叫處 :73-76 |
| 檔案不存在 | 讀成 null（用預設值，**不會**自動寫檔，除非觸發遷移） | `UserDataStore.cs:306-307` |
| BOM 輸入 | .NET 的 `Deserialize(Stream)` 會略過 UTF-8 BOM（我印象中如此）〔未實測〕；Rust 一律寫無 BOM，別賭 | — |
| 其他 sidecar | `explorer-menu.txt`（§5.4）、`logs\filee-YYYYMMDD.log`、`libreoffice-profiles\`、（FILEE_DATA_DIR 時）`engines\` | — |

**載入時的遷移與一致化**（`UserDataStore.cs:70-91`）：讀完四檔 → `SettingsMigrations.Apply`（settings）→ `ApplyToLibrary`（presets+profiles；只在 `profiles.json` 存在時）→ `EnsureConsistency`（:272-302）→ 若 `schemaVersion < 54` 立刻回寫 settings；若庫被遷移就回寫 presets+profiles。
`EnsureConsistency` 做的事：profile 的 `presetIds`／`backPresetIds` 去除不存在的 id、去重、最多 12 個（`ToolbarProfile.MaxSlices=12`）；`targetFormat=="group"` 的 preset 強制 `kind=Group`；group 的 `subPresetIds` 只留存在且非 group 者、去重、最多 6（`Preset.MaxSubPresets=6`）；沒有任何 `isFallback` profile 就補內建的 mixed。**這些只改記憶體，只有下次 SaveLibrary 才落地。**

**對 Rust 的規則**
- 看到 `schemaVersion > 54`（上游比你新）→ 不要寫 settings.json（可能丟欄位），只讀。
- 看到 `schemaVersion < 54` → 不要自己遷移；呼叫一次任意 CLI 指令（例如 `filee-cli.exe presets`，啟動就會 `Load()` 並回寫遷移結果）再重讀。
- 絕不寫 `.broken`、`.tmp` 以外的 sidecar；寫檔用同資料夾 `.tmp` + `MoveFileExW(REPLACE_EXISTING)`，與上游一致。

---

### 1.2 `settings.json`（`AppSettings`，`Core/Settings/AppSettings.cs:123-297`）

目前 `CurrentSchemaVersion = 54`（:126）。欄位與預設（括號內為本機實測值）：

| 欄位 | 型別 | 預設 | 備註 |
|---|---|---|---|
| `schemaVersion` | int | 54 | |
| `language` | string | `"auto"` | `auto`／`en`／`ko`／`zh-CN`…（`App/Assets/i18n/*.json` 有 16 種：ar de en es fr hi ja ko ne pl pt-BR pt-PT ru th tr zh-CN zh-TW） |
| `startWithSystem` | bool | true | 對應 Run 機碼（§5.4） |
| `contextMenuEnabled` | bool | true | |
| `checkForUpdates` | bool | true | |
| `closeAction` | enum `Ask`/`Tray`/`Quit` | `Ask` | **lenient**（未知值→`Ask`）（本機 `Tray`） |
| `keepHistory` | bool | true | |
| `historyLimit` | int | 200 | 有效值＝clamp(50, 2000)（:152-159） |
| `showInDock`／`showMenuBarIcon` | bool | true | macOS 用；讀取有 lenient 轉換器（:300-313） |
| `notifiedUpdateVersion` | string? | 省略 | |
| `whatsNewSeenVersion` | string? | 省略 | |
| `notifiedEngineVersions` | `{string:string}` | `{}` | |
| `paused` | bool | false | 托盤「暫停」，停用所有觸發手勢 |
| `theme` | object | 見下 | `style` `Soft`/`MaterialYou`；`mode` `System`/`Light`/`Dark`；`accent` `"#7F77DD"`；`presetKey` string?；`cornerRadius` 16；`reduceMotion` bool?（:36-58） |
| `donut` | object | 見下 | `outerRadius` 150；`holeRatio` 0.42；`opacity` 0.96（下限 0.1）；`flipKey` `"Tab"`（空字串＝無）；`flipWithWheel` true（:61-120） |
| `triggers` | `TriggerGesture[]` | 預設 2 個 | 見下 |
| `excludedProcesses` | string[] | `[]` | 不含 `.exe` 的程序名 |
| `enginePriority` | string[] | 29 個 id（見實測檔） | CLI 啟動時餵給 `ConverterCatalog.Priority`（`Cli/CliHost.cs:38`） |
| `firstRunCompleted` | bool | false | |
| `successfulConversions` | int | 0 | App 端用來決定何時問星星（`ConversionService.cs:233-242`）；Rust 可忽略 |
| `feedbackPromptShown`／`accessibilityPromptShown` | bool | false | |
| `watchFolders` | `WatchRule[]` | `[]` | §4.1 |
| `defaultOutput` | object | 見下 | `location` `SameFolder`/`Subfolder`/`CustomFolder`（`Default` 不該出現）、`subfolderName` `"converted"`、`customFolder` `""`（:329-337） |
| `browserDrops` | object | `{folder:"", saveOnlyConverted:false}` | 瀏覽器拖曳下載位置（空＝Downloads）（:340-347） |
| `engineOwnCopies` | `{string:string}` | `{}` | 使用者自備引擎路徑，CLI 啟動時套用（`CliHost.cs:32`） |
| `libreOfficeForSlides` | bool | true | |
| `libreOfficeForHwp`／`libreOfficeForDocuments`／`libreOfficeForSheets` | bool | false | |
| `keepFileDates` | bool | false | 全域覆寫：所有轉檔保留原檔日期 |
| `zipGeoResults` | bool | false | |
| `uiScale` | double | 1 | 0.7–1.8 |
| `mainWindow` | `{x?,y?,width?,height?,maximized}` | 省略 | |

**TriggerGesture**（`Core/Settings/TriggerGesture.cs:53-88`）：`id`（8 hex）、`enabled`(true)、`kind` `Drag`/`Hold`/`KeyChord`、`modifiers` **[Flags] enum 以字串逗號串接**：`"Ctrl"`、`"Ctrl, Alt"`（值 Ctrl=1,Alt=2,Shift=4,Meta=8；:7-16）、`button` `None`/`Left`/`Right`/`Middle`/`X1`/`X2`、`key`（SharpHook KeyCode 去掉 `Vc` 前綴，例 `"Space"`；省略＝null）、`dragThreshold`(0)、`holdMilliseconds`(450)、`scope` `FileManager`/`Anywhere`。預設兩個：`Drag + Ctrl + Left`、`KeyChord + "Ctrl, Alt" + None + "Space"`。

`WithDefaultOutput`（:276-296）規則（CLI 與 App 都在跑任務前套用，Rust 若自行決定輸出位置要重現）：preset 的 `output.location == Default` 時 → 用 `defaultOutput.location`（若它也是 Default 當 SameFolder）、`subfolderName`、`customFolder`，並把 `subfolderOnlyForSeveralFiles` 設 false；`keepFileDates` 開且 preset 沒開 → `output.keepDates=true`；`zipGeoResults` 開且目標格式屬 Geo 類別 → `output.zipResult=true`。

---

### 1.3 `presets.json`（`List<Preset>`，`Core/Presets/Preset.cs:83-285`）

頂層是 JSON 陣列。本機 136 筆：125 `Convert`、8 `Tool`、3 `Group`。**建議 Rust 把 Preset 當 opaque**：只解析下列欄位，其餘 `image/pdf/document/media/archive/trace/cad` 整塊原樣保留／傳遞。

| 欄位 | 型別 | 預設／規則 |
|---|---|---|
| `id` | string | 12 hex（`NewId()`，:222 附近）；內建為 `to-png` 之類固定字串 |
| `name` | string | 使用者名稱；非空則優先於 `nameKey`（`Core/Localization/ILocalizer.cs:40-43`） |
| `nameKey` | string? | 內建 preset 的在地化鍵（如 `preset.crop`）；null 時省略 |
| `targetFormat` | string | `FormatRegistry` 的格式 id；`"*"`＝同來源格式（`BuiltInData.SameAsSource`，`Core/Presets/BuiltInData.cs:12`）；group 固定 `"group"`（:130） |
| `kind` | enum `Convert`/`Group`/`Tool` | `Convert`；**lenient，未知值→`Group`**（:58-61） |
| `tool` | string? | `Tool` 才有意義：`null`/`"crop"`（舊）、`"clip"`、`"image-edit"`、`"hide"`（:229-235）；本機另有 `tool-sheet-studio`／`tool-pdf-crop`／`tool-pdf-pages`／`tool-pdf-add` 這幾筆 `kind=Tool` 無 `tool` 欄 |
| `subPresetIds` | string[] | Group 用，≤6、非 group |
| `output` | `OutputRule` | 見下 |
| `image` `pdf` `document` `media` `archive` `trace` `cad` | object | 各自的選項類別：`ImageOptions`:404、`PdfOptions`:719、`DocumentOptions`:815、`MediaOptions`:840、`ArchiveOptions`:1120、`TracingOptions`:1140、`CadOptions`:1189。欄位全集請直接看本機 presets.json 任一筆（所有非 null 欄位都會寫出） |

選項類別裡 Rust 可能想讀的：`pdf.mergeIntoSingle`（Merge PDF）、`pdf.password`（歷史紀錄不得保存，見 §2）、`image.joinIntoOne`、`image.multiPageTiff`、`media.joinIntoOne`/`slideshow`、`archive.combineIntoOne`（這些決定「一個 job 的多檔會合成一個輸出」，`Preset.cs:150-215`、`AskedLocation.MakesOneResult` `Core/Presets/AskedLocation.cs:12-19`）。

**OutputRule**（`Core/Presets/OutputRule.cs:33-124`）：

| 欄位 | 型別／預設 |
|---|---|
| `location` | enum `Default`/`SameFolder`/`Subfolder`/`CustomFolder`；**lenient→`Default`**（:35-37） |
| `askEveryTime` | bool false |
| `subfolderName` | `"converted"`（可含 token） |
| `subfolderOnlyForSeveralFiles` | bool false |
| `customFolder` | `""` |
| `keepFoldersFrom` | `""`（見 §3.4） |
| `fileNamePattern` | `"{name}"` |
| `renames` | `RenameStep[]`：`{kind, find, replace}`，`kind` ∈ `Replace`/`Remove`/`Prefix`/`Suffix`/`SpacesToUnderscores`/`RemoveCopyNumber`/`RemoveBrackets`/`RemoveLeadingNumber`/`RemoveSymbols`/`Lowercase`/`Uppercase`/`Regex`（:127-197） |
| `conflict` | enum `Rename`/`Overwrite`/`Skip`，預設 `Rename` |
| `keepDates`／`zipResult`／`onlyIfSmaller`／`replaceOriginal` | bool false |
| （`confirmedPath`、`sourceFolder`） | `[JsonIgnore]`，**不會**出現在檔案裡 |

---

### 1.4 `profiles.json`（`List<ToolbarProfile>`，`Core/Profiles/ToolbarProfile.cs:10-61`）

| 欄位 | 型別 | 說明 |
|---|---|---|
| `id` | string | 12 hex 或內建（`images`、`pdf`、`mixed`…；本機 19 個） |
| `name` | string | 使用者名稱，空字串→用 `nameKey` |
| `nameKey` | string? | 如 `profile.images` |
| `extensions` | string[] | 小寫、無點。一般 profile＝它負責的副檔名；`isFallback` profile＝混合選取必須落在其內（空＝任意）（:22-27） |
| `isFallback` | bool | 「混合檔案」profile |
| `presetIds` | string[] | 甜甜圈正面，順時針自 12 點，≤12 |
| `backPresetIds` | string[]? | 背面（Tab 翻面）；null＝舊檔無背面，載入時內建 profile 會補預設（`SettingsMigrations.AddBackFaces`），其他視為空 |

選 profile 的規則（`ProfileSelector.Select`，:74-90）：所有檔案的副檔名集合 → 第一個「非 fallback 且含全部副檔名」的 profile → 否則第一個「fallback、`extensions` 非空且含全部」→ 否則 catch-all（第一個 `isFallback && extensions 空`，再退而求其次第一個 fallback，再第一個 profile，:97-100）。

---

### 1.5 `history.json`（`List<HistoryEntry>`，`Core/History/HistoryEntry.cs:9-121`）

頂層陣列，**最新在前**（`History.Insert(0, entry)`，`UserDataStore.cs:136`）。屬性寫出順序＝宣告順序：

| 欄位 | 型別 | 說明 |
|---|---|---|
| `jobId` | string | 10 hex（`Guid.NewGuid().ToString("N")[..10]`，`ConversionJob.cs` 的 `Id`）；工具視窗寫的用 12 hex |
| `finishedAt` | DateTime | `DateTime.Now`（本地時間）。.NET 對 Local Kind 會寫成帶位移的 ISO 8601，例 `2026-10-29T01:44:12.1234567+08:00`〔讀碼推論，未實測 history 內容；本機 history.json 目前是 `[]`〕 |
| `presetId` | string | |
| `presetName` | string | **當下 UI 語言**的顯示名稱（`loc.DisplayName(preset)`）：`name` 非空取 `name`，否則 `loc[nameKey]`，否則 `targetFormat.ToUpper()` |
| `state` | enum `JobState` | `Queued`/`Running`/`Completed`/`CompletedWithErrors`/`Failed`/`Cancelled`（`Core/Conversion/ConversionJob.cs:7-15`）；歷史只會出現後四種 |
| `sources` | string[] | 來源完整路徑 |
| `outputs` | string[] | 所有輸出（每檔 outputs 攤平） |
| `watchRuleId` | string? | 來自自動轉換資料夾時＝`WatchRule.id`，否則省略 |
| `failures` | `HistoryFailure[]` | `{source, errorKey?, errorDetail?}` |
| `preset` | Preset? | **只有失敗時**才存（整份 preset 副本，已套用存檔位置，PDF 密碼清空並 `pdf.passwordLeftOut=true`），讓 Retry 用；無失敗時省略（`HistoryEntry.cs:47-69` From()） |
| `errors` | string[] | 1.7 以前舊格式，新紀錄為 `[]` |
| `undoable` | `HistoryOutput[]` | `{path, length, writtenUtc, trashed}`；Undo 的依據 |
| `originalsInTrash` | bool | Replace original 把來源丟進資源回收筒 |
| `undone` | bool | |

`HistoryOutput`（:106-121）：`path`、`length`(long)、`writtenUtc`（`LastWriteTimeUtc`，**以 100ns 精度逐字相等比對**，所以 Rust 要用 FILETIME 精確換算：.NET ticks ＝ FILETIME + 504911232000000000，輸出 7 位小數秒＋`Z`）、`trashed`(false)。`Unchanged()`＝檔案存在且 Length 與 LastWriteTimeUtc 都相同。

---

### 1.6 並行寫入風險與分工建議

事實：
1. Filee.exe **記憶體內持有整份 store，存檔時整檔覆寫**（`SaveSettings`、`SaveLibrary`、`AddHistory`、`SaveUiScale`、`SaveMainWindow` 全是 `Write(path, 記憶體)`，`UserDataStore.cs:93-161`）。它從不重讀 → **Rust 在 Filee.exe 開著期間寫的任何東西，會在 Filee.exe 下一次存檔時被蓋掉**（最常見觸發：關閉主視窗存 `mainWindow`、縮放存 `uiScale`、`CountSuccess` 存設定、通知更新存 `notifiedUpdateVersion`）。
2. 沒有跨進程鎖；`.tmp` 檔名固定（`path + ".tmp"`），兩個進程同時寫同一檔會互踩 tmp。
3. CLI 啟動即 `Load()`，若檔案是舊 schema 會**回寫**（`UserDataStore.cs:84-90`）。

建議（所有權表）：

| 檔案 | Rust 可寫 | 上游 Filee.exe（設定視窗）可寫 | 守則 |
|---|---|---|---|
| `settings.json` | 只改 `paused`（托盤暫停）、`watchFolders`（若 Rust 自己提供編輯 UI，否則唯讀）、`language`；**其餘不動** | 全部（設定視窗） | Rust 改檔前確認沒有子程序 Filee.exe 在跑（Rust 自己啟動的，持有 PID），改完即時重載；Filee.exe 結束後重讀整份 |
| `presets.json`／`profiles.json` | **唯讀**（Rust 沒有編輯器） | 全部 | Filee.exe 結束後（`Child.wait()` 回來）重讀；用檔案 mtime 當快取失效條件 |
| `history.json` | 追加（轉完一個 job 就寫一筆）、`ClearHistory` 等價操作 | 開著時 Home 頁會 `AddHistory` 它自己做的轉檔（工具視窗） | 寫入採「讀最新 → 插入頭 → 截到 `historyLimit` → tmp+rename」，讀改寫之間不要跨 await；設定視窗開著時**延後**寫入（記在記憶體佇列），視窗關閉後補寫，否則會被 Filee.exe 的記憶體版本蓋掉 |

CLI 子程序與 Rust 同時存在沒問題（CLI 只讀，必要時寫遷移）；唯一例外是 schema 遷移，見 §1.1。

---

## 2. history.json 一筆紀錄怎麼產生

### 2.1 App 的做法（`App/Services/ConversionService.cs:213-230`）

`JobQueue.JobFinished` → `HistoryEntry.From(job)`（`HistoryEntry.cs:47-69`）→ 補 `WatchRuleId`（來自 `_watchJobs`）→ `UserDataStore.AddHistory`（`UserDataStore.cs:128-142`）：

```
if (!settings.keepHistory) return;                 // 完全不記
limit = clamp(settings.historyLimit, 50, 2000)
history.Insert(0, entry);
if (history.Count > limit) history.RemoveRange(limit, Count - limit);   // 砍最舊的
write history.json (atomic)
```

`HistoryEntry.From` 的內容規則：
- `state` = job.State（規則見 §3.1）；`sources` = `job.Sources`；`outputs` = `job.Outputs`（每檔輸出攤平）。
- `failures` = 每個 `FileState.Failed` 的檔案 → `{source, errorKey, errorDetail}`（**Cancelled 不進 failures**）。
- `preset` = 有失敗且 `!image.hidesAreas` 時才存 `preset.Clone().WithoutPassword(mark:true)`（`Core/Presets/Preset.cs:240-252` 附近 `WithoutPassword`）。
- `undoable`（:77-102）：逐檔逐輸出；輸出若等於來源路徑（Only-if-smaller + Replace original 保留來源）跳過；**只要有任何輸出不是檔案（資料夾、Extract）、或旁邊存在 `<檔名>_files` 資料夾，整個 job 的 undoable 回 `[]`**；以路徑去重（Windows 不分大小寫）；超過 `MaxUndoable=500` 個也回 `[]`。每筆記 `Length`＋`LastWriteTimeUtc`。
- `originalsInTrash` = 任一檔 `OriginalInTrash`。

### 2.2 Rust 用 CLI 轉完後怎麼寫（建議流程）

CLI `--json` 輸出（§3.5）提供每檔 `source/state/outputs/error/notice/missingFonts`，足以重建：

| history 欄位 | 來源 |
|---|---|
| `jobId` | Rust 自產 10 hex |
| `finishedAt` | Rust 取 `DateTime.Now` 等價（本地時間＋位移，7 位小數） |
| `presetId` | 你傳給 CLI 的 `--preset` id（傳 `--to` 時無 preset：用 `"cli"`？→ 建議**一律用 `--preset`**，沒有 preset 的情境才用 `--to` 並寫 `presetId:""`） |
| `presetName` | Rust 用 `settings.language` 對應的 `i18n/<lang>.json` 求值（`name` 優先，其次 `nameKey`，再 `targetFormat.ToUpper()`）。CLI JSON 的 `"preset"` 是**英文**名稱，不要直接用 |
| `state` | 由各檔 state 推：被取消→`Cancelled`；全部 `done`/`skipped`→`Completed`；有 `done` 但也有失敗→`CompletedWithErrors`；否則`Failed`（照搬 `JobQueue.cs:134-137`） |
| `sources` | JSON `files[].source` |
| `outputs` | JSON `files[].outputs` 攤平（合併類 preset：輸出歸屬第一個檔，§3.5） |
| `failures[]` | `state=="failed"`：`source`；`error` 字串是 `"<英文訊息> (<細節>)"`——Rust 內嵌 `en.json`，用 `error.*` 的值去前綴比對可還原 `errorKey`＋`errorDetail`（`FailureText.Reason`＝`loc[key ?? "error.conversion_failed"] + " (detail)"`，`App/ViewModels/FailureText.cs:12-13`）；比對不到就 `errorKey:null`＋`errorDetail:<整串>` |
| `preset`（僅失敗時） | 設定視窗的 Retry 要這個（`HomePageViewModel.cs:59`：`entry.Preset is not null` 才顯示 Retry）。可寫實際執行的 preset 副本（已套 `WithDefaultOutput`、`-o`、PDF 密碼清空＋`passwordLeftOut:true`）；**寫 `null` 也合法**（只是 Retry 按鈕不出現） |
| `watchRuleId` | 自動轉換資料夾的 `rule.id` |
| `undoable` | Rust 在 CLI 結束後對每個輸出 `stat`：`length`＋`writtenUtc`，照 §2.1 的排除規則；不想做 Undo 就寫 `[]`（Undo 按鈕不出現） |
| `originalsInTrash` | JSON 沒有這個資訊 → 只有 preset `replaceOriginal=true` 且來源檔已不存在時才可推測為 true；不確定就 false（Undo 只是少一個說明） |

寫入前檢查 `settings.keepHistory`；插入頭、截到 `clamp(historyLimit,50,2000)`；Filee.exe 設定視窗開著時延後（§1.6）。上游「Recent conversions」頁就是讀這份，所以格式正確即可立刻顯示（下次設定視窗啟動時載入）。

---

## 3. 轉檔流程（ConversionService / JobQueue / CLI）

### 3.1 一個 job 的生命週期

`ConversionService.Start`（`ConversionService.cs:69-92`）：`files` 空或 preset 是 group/tool → 不開工；先 `settings.WithDefaultOutput(preset)`；有瀏覽器拖曳（WebDrop）則裝 `WebDropFetcher`；`JobQueue.Enqueue` → 回傳 `JobViewModel`，插入 `Jobs`（及 Toast）。

`JobQueue`（`Core/Conversion/JobQueue.cs`）：
1. `Enqueue` → `new ConversionJob(sources, preset, displayName, fetcher)`：`Preset.Clone()` 凍結（任務中改 preset 不影響）、Shapefile 的 `.dbf/.shx/.prj…` 伴隨檔被剔除（`ConversionJob.cs` 建構子）；state=`Queued`。
2. `RunJobAsync`（:79-160）：state=`Running` → 工作資料夾 `%TEMP%\Filee\<jobId>` → （有 fetcher 先存檔，佔進度前 50%）→ 規劃路由（Dijkstra，最多 3 hop）→ 依模式分支：媒體串接／圖片合併／PDF 合併（`pdf.mergeIntoSingle && target==pdf && files>1`）／多頁 TIFF／打包成單一壓縮檔／一般逐檔（`Parallel.ForEachAsync`，`MaxDegreeOfParallelism = Environment.ProcessorCount`，另有各引擎的 gate，如 archive 2、CAD ≤4）。
3. 結束狀態（:134-137）：被取消→`Cancelled`；全部檔 `Done|Skipped`→`Completed`；有 `Done` 但有別的→`CompletedWithErrors`；其餘→`Failed`；未預期例外→`Failed`。
4. `finally`：釋放輸出名稱保留、清 `%TEMP%\Filee\<jobId>`、`Release(job)`；最後 `Notify` + `JobFinished`。

檔案狀態 `FileState`：`Pending`/`Running`/`Done`/`Skipped`/`Failed`/`Cancelled`（`ConversionJob.cs:17-25`）；`FileResult.Progress` 0..1，`ConversionJob.Progress` = 各檔平均（:88）。

### 3.2 進度回報粒度、取消

- **引擎內部進度**：App 內 `JobUpdated` 事件頻繁觸發（`ConversionService.cs:206-211` 用 coalesce 合併到 UI 執行緒 Background 優先序），粒度是每檔 0..1 的 `Progress`。
- **CLI 完全沒有進度輸出**。〔實測〕JSON 模式只在結束時印一次；人類可讀模式每檔完成時印一行（`Cli/Cli.cs:123-151` `RunJobAsync` 的 `Print`，掛在 `JobUpdated`，只印「已不是 Pending/Running」的檔）。實測 6 張圖 AVIF：6 行在 1.63–1.86 s 內陸續印出，**順序＝完成順序（平行）**，最後一行 `Converted 6 of 6 file(s).`。
- **Rust 顯示進度的做法（建議）**：
  1. 一律用**人類可讀模式**（不加 `--json`）讀 stdout 逐行（UTF-8，CLI 已 `Console.OutputEncoding = UTF8`，`Cli/Program.cs:7`）；以「已完成檔數 / 總檔數」當進度條，單檔 job 用不定進度。
  2. 行格式（`Cli.cs:153-165`）：`ok    <src> -> <out1>, <out2>[ (notice)][ (missing fonts: …)]`、`skip  <src> (the output file already exists)`、`stop  <src> (cancelled)`、`fail  <src>: <message>`；最後一行 `Converted N of M file(s)[, K failed].`。路徑會**縮成相對於 cwd**（`Shorten`，`Cli.cs:176-181`；在 cwd 之內才縮）→ Rust 啟動時固定 cwd，行首狀態字（`ok`/`skip`/`stop`/`fail`，後接空白）+ 逐一比對你送進去的來源清單前綴，即可歸屬到檔案；輸出路徑含 `, ` 或 ` -> ` 時有歧義。
  3. 需要「權威輸出清單」時，改走 `--json`（放棄逐檔進度）；折衷：一般用人類模式，轉完以檔案系統確認輸出，或對 ≤1 檔的 job 用 `--json`。兩者無法同時要。
  4. 要更細粒度（單一大檔的百分比）→ CLI 做不到；只能不定進度＋已耗時。
- **取消**：CLI 的 `Console.CancelKeyPress`：第一次 Ctrl+C → `job.Cancel()`（引擎盡快停、刪除自己寫到一半的輸出）；第二次才立即結束（`Cli/Program.cs:9-15`）。Rust 要「優雅取消」就對 CLI 送 Ctrl+C／Ctrl+Break（子程序用 `CREATE_NEW_PROCESS_GROUP`＋`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)`；.NET 的 `CancelKeyPress` 也接 Break）〔**未實測**：此機器禁止 `Add-Type` 編譯 P/Invoke，沒驗證〕。退路是 `TerminateProcess`——此時半成品輸出不會被清，需 Rust 自己清理，且 `%TEMP%\Filee\<jobId>` 會殘留（App 啟動時用 `DeleteStaleWorkFolders` 清過期的，`JobQueue.cs:1308`）。

### 3.3 CLI 與 App 的分工（哪些只要傳 preset 就有）

| 功能 | CLI（傳 `--preset`） | App 端才有 |
|---|---|---|
| 路由規劃、引擎優先序、多檔合併（Merge PDF/圖片/影音/TIFF/壓縮檔） | ✅（同一個 JobQueue） | — |
| 輸出路徑規則（pattern／renames／conflict／subfolder／KeepFoldersFrom） | ✅（`OutputPathResolver`） | — |
| `settings.defaultOutput`、`keepFileDates`、`zipGeoResults` | ✅（`ResolvePreset` 呼叫 `WithDefaultOutput`，`Cli.cs:270`） | — |
| Replace original（來源進資源回收筒） | ✅（`CliHost.cs:39-41` 傳入 `Trash()` = `WindowsPlatformServices.MoveToTrash`；`ARCHITECTURE.md` 明載「in the app and the command line」） | watch folder／瀏覽器檔永不替換 |
| PDF 密碼、浮水印、頁碼、OCR | ✅（preset 內） | — |
| MissingFonts | ✅**只回報**：人類模式 `(missing fonts: …)`、JSON `missingFonts[]`（`Cli.cs:153-165`、`Cli.cs:207-213`）；不暫停、不安裝 | Toast 卡片可提供「受信任來源」安裝字型 |
| `{preset}` token | ⚠ **用英文名稱**（`CliHost.cs:25` `EnglishTexts`；`Cli.cs:126`） | 用 UI 語言名稱 |
| `output.askEveryTime`（每次詢問存檔位置） | ❌ 直接用 `location`（`OutputRule.cs:40-44` 註解明說「command line 無法詢問」） | 甜甜圈放開後彈存檔對話框（`RadialController.cs:476`、`AskedLocation.ForFile/ForFolder`） |
| 瀏覽器拖曳（virtual file／URL／blob） | ❌ | `WebDrop`/`WebDownloader`（`ConversionService.cs:69-92`、`BrowserDropOptions` :156-161；佔位檔在 `%TEMP%\Filee\web\<drop>\drop\<n>\`） |
| 互動工具（Crop、Cut a clip、Edit pictures、Pixelate/blur、PDF 頁面編輯、試算表列印工作室） | ❌（`found.IsTool` → 退出碼 2，`Cli.cs:263-266`） | 各自視窗；**這些工具自己呼叫 `AddHistory`**（`CropOverlayViewModel.cs:474`、`PdfPageEditorViewModel.cs:357`、`SpreadsheetPrintStudioViewModel.cs:323`）。Filee.exe 沒有「直接開某工具」的命令列參數，只有 `--convert <檔>` 開甜甜圈（`CommandLine.cs:5-9`） |
| Undo（輸出丟資源回收筒）、Retry、Toast、成功三次後的星星提示 | ❌ | `HomePageViewModel`、`ConversionService.CountSuccess` |
| 寫 history | ❌ | `ConversionService.OnJobFinished` |

注意：preset 若 `kind=Group` → CLI 退出碼 2 並印出成員 id（`Cli.cs:254-262`）；`--preset` 先比對 id（不分大小寫），再比對**英文**顯示名稱，且「轉換類」優先於 group/tool（`Cli.cs:243-248`）。


### 3.4 輸出路徑規則（`OutputPathResolver.Resolve`，`Core/Presets/OutputPathResolver.cs:24-65`，檔內實際行號）

輸入：`OutputRule`、tokens（來源路徑、preset 顯示名、批次內 1 起算的 index、現在時間）、目標副檔名、`exists(path)`（磁碟上存在**或本批已保留**）、可選 `suffix`（如分頁 `_p3`）。

1. 來源路徑去掉結尾分隔符；`sourceDir` = `rule.SourceFolder`（瀏覽器檔專用，不存檔）否則來源所在資料夾。
2. 目錄：`Subfolder` → `sourceDir\<Sanitize(ExpandPattern(subfolderName), "converted")>`；`CustomFolder` 且路徑非空白 → `customFolder\<SubfolderBelow(keepFoldersFrom, sourceDir)>`；其餘（`SameFolder`、`Default`、空的 CustomFolder）→ `sourceDir`。`SubfolderBelow`（:71-79）：`keepFoldersFrom` 為空→空；來源資料夾等於該根、或在其外（相對路徑以 `..` 開頭／有磁碟機）→ 空；否則回相對子路徑（`Inbox\2025\a.jpg` → `converted\2025\a.pdf`）。
3. 檔名 = `Sanitize(Rename(ExpandPattern(fileNamePattern), renames), 來源檔名) + suffix`；token 只有 `{name}` `{ext}`（來源副檔名）`{preset}` `{date}`(`yyyyMMdd`) `{time}`(`HHmmss`) `{index}`，**一次掃描**、不分大小寫、未知 token 原樣保留（:120-138）；`Sanitize`（:142-151）把 `<>:"/\|?*` 與控制字元換成 `_`、去頭尾空白與結尾 `.`，結果空則用來源檔名。副檔名由目標格式決定（空＝資料夾輸出，如 Extract）。
4. 衝突：候選路徑若**就是來源本身**→ 一律走 Rename（來源永不被覆蓋）；不存在→直接用；`ConfirmedPath`（每次詢問的存檔對話框）相同→直接用；`Overwrite`→用原名；`Skip`→回 null（job 內該檔 `FileState.Skipped`）；`Rename`→`<name> (2)`、`(3)`…。
5. 子資料夾「僅多檔時」（`subfolderOnlyForSeveralFiles`）、`zipResult`、`onlyIfSmaller`、`keepDates`、`replaceOriginal`、分頁／影格後綴等由 `JobQueue` 處理（`SubfolderOnlyForSeveralFiles` 的說明：只有一個來源變多個檔，如 PDF 分頁，才進子資料夾，`OutputRule.cs:55-60`）。
6. 失敗或被取消的轉換會刪掉自己寫了一半的輸出（僅「該路徑原本不存在且是自己獨佔」的），並移除自己建立且空了的資料夾（`ARCHITECTURE.md` 流程第 5 點）。

CLI `-o <folder>`：`UseFolder` 把 preset 改成 `location=CustomFolder`、`customFolder=<完整路徑>`（`Cli.cs:282-286`）——**不改 `keepFoldersFrom`／`fileNamePattern`／`renames`**。所以「保留子資料夾結構」「改檔名」這些無法只靠 CLI 旗標達成，見 §4.3 的臨時 preset 作法〔實測可行〕。

### 3.5 `filee-cli convert` 實測輸出（v2.2.0，`C:\Program Files\Filee\filee-cli.exe`）

所有測試都設 `FILEE_DATA_DIR` 指向 scratchpad 內複製來的資料夾，**沒有碰 `%APPDATA%\Filee`**。

**指令列**（`Cli/CliOptions.cs:27,29-133`）：`filee convert <files|folders|wildcards…> (--to <format> | --preset <id|name>) [-o folder] [-r] [-q 1-100] [--overwrite|--skip] [--keep-dates] [--zip] [--json] [--quiet]`；`filee watch <folder> (--to|--preset) [-o] [-r] [--move-originals] [--settle <sec>] [--quiet]`；另有 `formats [<fmt>]`、`presets`、`help`、`version`。`--to` 與 `--preset` 必須二選一。退出碼（`Cli.cs:17-20`）：0 全成功、1 有失敗、2 參數錯或找不到輸入、3 沒有可轉的檔。

**(a) 成功（`--json`，兩個 PNG → JPG）**
```
$ filee-cli convert in\a.png in\b.png --preset to-jpg --json        exit=0
{
  "preset": "JPG",
  "target": "jpg",
  "converted": 2,
  "skipped": 0,
  "failed": 0,
  "files": [
    { "source": "C:\\...\\in\\a.png", "state": "done", "outputs": [ "C:\\...\\in\\a.jpg" ] },
    { "source": "C:\\...\\in\\b.png", "state": "done", "outputs": [ "C:\\...\\in\\b.jpg" ] }
  ]
}
```
（實際輸出為縮排 JSON，每個欄位各一行；上面只是為了篇幅摺行。**路徑一律是完整路徑**、`\` 以 `\\` 跳脫；`Utf8JsonWriter` 預設編碼器會把 `` ` `` 與 `'` 輸出成 `\u0060`、`\u0027`，標準 JSON parser 沒問題。）

**(b) 失敗＋略過（`--skip`，a.jpg 已存在、bad.png 不是圖）**
```
$ filee-cli convert in\a.png in\bad.png --preset to-jpg --skip --json   exit=1   (0.40 s)
{ "preset":"JPG","target":"jpg","converted":0,"skipped":1,"failed":1,
  "files":[
   {"source":"…\\a.png","state":"skipped","outputs":[]},
   {"source":"…\\bad.png","state":"failed","outputs":[],
    "error":"The engine reported an error (improper image header \u0060…\\bad.png\u0027 @ error/png.c/ReadPNGImage/3956)"} ] }
```
`state` 取值：`done`/`skipped`/`failed`/`cancelled`（`file.State.ToString().ToLowerInvariant()`，`Cli.cs:198`）；`failed` 計數＝Failed+Cancelled（:191）；失敗/取消有 `error`（英文），成功且有備註有 `notice`（英文），有缺字型有 `missingFonts[]`（:203-213）。**JSON 只寫到 stdout；錯誤訊息（usage、not found）寫 stderr，格式 `filee: …`**。

**(c) 合併類 preset（Merge PDF，2 檔→1 個 PDF）**
```
$ filee-cli convert in\a.png in\b.png --preset merge-pdf -o out2 --json   exit=0
{ "preset":"Merge PDF","target":"pdf","converted":2,"skipped":0,"failed":0,
  "files":[ {"source":"…\\a.png","state":"done","outputs":["…\\out2\\a_merged.pdf"]},
            {"source":"…\\b.png","state":"done","outputs":[]} ] }
```
→ 合併輸出掛在**第一個**檔下，其餘檔 `done` 但 `outputs:[]`；輸出檔名為 `<第一檔>_merged.pdf`。

**(d) 人類可讀模式**
```
$ filee-cli convert in\a.png in\b.png --preset to-webp -o out1          exit=0
ok    in\b.png -> out1\b.webp
ok    in\a.png -> out1\a.webp
Converted 2 of 2 file(s).
$ filee-cli convert in\bad.png --to jpg                                 exit=1
fail  in\bad.png: The engine reported an error (improper image header `…' @ error/png.c/ReadPNGImage/3956)
Converted 0 of 1 file(s), 1 failed.
```
（`--quiet`：什麼都不印，只剩退出碼。）

**(e) 其他實測**
```
$ filee-cli convert in\a.png in\nonexist.png --to jpg --json     → stderr "filee: not found: in/nonexist.png"  exit=2（stdout 無 JSON）
$ filee-cli convert empty --to jpg                               → stderr "filee: no files to convert."  exit=3
$ filee-cli convert in\a.png --preset rotate                     → stderr "filee: 'rotate' is a group on the donut, not a conversion. Use one of: rotate-left, rotate-right, rotate-180"  exit=2
$ filee-cli convert in\a.png --preset tool-crop                  → stderr "filee: 'tool-crop' is an interactive tool, not a conversion."  exit=2
```
`filee-cli presets` 以固定寬欄位列出非 group/非 tool 的 preset：`ID(24) NAME(28) TARGET`（本機列出至 `to-txt` 以後還有更多）。

**(f) 臨時 preset 作法（取代 CLI 沒有的旗標）〔實測〕**：複製 `settings.json`/`profiles.json`/`history.json` 到臨時資料夾，並把 `presets.json` 附加一筆改過 `output` 的 preset（`location:"CustomFolder"`、`customFolder`、`keepFoldersFrom`、`fileNamePattern:"{name}_x"`、`renames:[{kind:"Uppercase"}]`），以 `FILEE_DATA_DIR=<臨時資料夾>` 跑 `convert … --preset eph-1`：
```
ok    wf\sub\deep.png -> wfout\sub\DEEP_X.jpg
ok    wf\top.png -> wfout\TOP_X.jpg
```
子資料夾結構、檔名 pattern、renames 全部生效。**坑**：設了 `FILEE_DATA_DIR` 後，下載式引擎的根目錄變成 `<該資料夾>\engines`（`src/Filee.Engines/Infrastructure/EngineEnvironment.cs:25-30`），而不是 `%LOCALAPPDATA%\Filee\engines`；`libreoffice-profiles` 與 logs 也隨資料夾走。本機 `%LOCALAPPDATA%\Filee\engines` 目前只有 `ocr-languages`（`searchable-pdf` 用得到）；臨時資料夾要放一個指向它的目錄連結（junction）才找得到。內建引擎（隨安裝在 `C:\Program Files\Filee\engines\{7zip,rhwp}`）不受影響。junction 的建立請依防毒規則先問使用者；或改成「Rust 把臨時 preset 直接加進真正的 presets.json」（會污染使用者庫，**不建議**）。

---

## 4. 自動轉換資料夾

### 4.1 `WatchRule`（`settings.json` → `watchFolders[]`，`Core/Watching/WatchRule.cs:19-88`）

| 欄位 | 型別／預設 | 說明 |
|---|---|---|
| `id` | string（8 hex） | 歷史紀錄的 `watchRuleId`、狀態檔檔名來源 |
| `enabled` | bool true | App 只處理 `enabled && folder 非空白 && presetId 非空白` 者（`WatchFolderService.cs:45-46`） |
| `folder` | string `""` | 被監看的資料夾 |
| `presetId` | string `""` | 必須是**轉換類**（`FindConversion`：非 group/tool，`UserDataStore.cs:236`）；找不到就整批略過並記 warning（`WatchFolderService.cs:76-85`） |
| `includeSubfolders` | bool false | |
| `outputFolder` | string `""` | 空＝`<folder>\converted`（`ResolvedOutputFolder` :55-56） |
| `originals` | enum `Keep`/`MoveToOriginals` | `MoveToOriginals`＝轉完把來源搬到 `<folder>\originals`（:59） |
| `include` | string[] `[]` | 檔名過濾，見下；空＝全部 |
| `fileNamePattern` | string `""` | 非空則蓋掉 preset 的 pattern |
| `renames` | `RenameStep[]` `[]` | 附加到 preset 的 renames 後面（空白步驟 `IsBlank` 跳過） |

`WatchRule.Apply(preset)`（:66-79）= `preset.Clone()` + `output.location=CustomFolder`、`output.customFolder=ResolvedOutputFolder`、`output.keepFoldersFrom=Folder`、**`output.replaceOriginal=false`**、（非空則）`fileNamePattern`、`renames.AddRange(非空白 renames)`。

`FileNameFilter`（`Core/Watching/FileNameFilter.cs`）：每個 pattern 都不分大小寫；`/…/` 夾住的是正規表達式（長度>2、頭尾都是 `/`）；其餘 `*`→`.*`、`?`→`.`、其他字元跳脫，前後加 `^…$`；任一符合即過；無效 pattern 被略過，全部無效視為「全部」；250 ms regex 逾時視為不符。`Normalize`（UI 輸入用，:28-39）：`heic`→`*.heic`（需是已知副檔名）、不含 `*?` 的文字→`*文字*`。

### 4.2 FolderWatcher 的穩定判斷（`Core/Watching/FolderWatcher.cs`，行號為檔內實際行號＝上列輸出行號 −88）

- `FileSystemWatcher(Folder)`：`IncludeSubdirectories=includeSubfolders`、`NotifyFilter = FileName|Size|LastWrite`、`InternalBufferSize=64KB`；事件 `Created/Changed/Renamed` 都呼叫 `Notice(path)`；`Error`（緩衝區溢位）→ `ScanExisting()` 重掃（:84-121）。
- **啟動時只有 `originals==MoveToOriginals` 才掃既有檔案**（它們是「沒人看著時到的」）；`Keep` 只處理新到的（:116-117 附近），因為舊的已轉過。
- `Notice`（:124-129）：`ShouldIgnore` 就丟棄；否則記 `Candidate{存在, Length, LastWriteUtc, StableSince}`。
- **每 1 秒**（`_time.CreateTimer(..., 1s, 1s)`，:118）跑 `ProcessPendingAsync`（:136-208）。對每個候選：重新 stat（長度或 mtime 變了→`StableSince` 重設）；不存在或已被 `GeneratedWatchOutputs` 認定是自己的產物→移出；要「穩定 ≥ `settleTime`」＝**Windows 2 秒**（macOS/Linux 5 秒；`DefaultSettleTime`，:28；CLI `--settle` 可覆寫）、**且 `CanOpenExclusively`（`FileShare.None` 開得起來，:294）**、（macOS 還要 Finder 複製中旗標 1984-01-24）才算 ready。
- ready 的檔先以 `key = path|length|lastWriteTicks` 查 `_handled`（記憶體集合）去重 → 同版本不重轉；排序後一次交給 `_convert`（一批一個 job）。一次只跑一批（`_busy` 旗標；忙時新檔等下一次 tick）。
- 批次結束：把 `job.Outputs` 記進 `GeneratedWatchOutputs`（見下）與 `_handled`；`originals==MoveToOriginals` 時，`Done` 的來源搬到 `<folder>\originals\<相對路徑>`，重名加 ` (2)`（`MoveToOriginals`，:259-277；來源不在 folder 內或已在 originals 內則不動）。
- `ShouldIgnore`（:212-239）排除：檔名空、以 `~$` 或 `.` 開頭、`desktop.ini`/`thumbs.db`、副檔名 `.tmp .temp .part .partial .crdownload .download .opdownload .!ut .filepart`；輸出資料夾**專用子樹**內的檔（輸出等於 folder 或其父層時不排除整個 root，issue #234）；`originals` 內；`includeSubfolders=false` 時不在 folder 本層；`FormatRegistry.Detect(path)==null`（不是 Filee 認得的格式）；不符合 `include`；目錄／隱藏／系統屬性。

**`.filee-watch-*.json` 狀態檔**（`GeneratedWatchOutputs.cs`，檔內行號＝輸出行號 −118）：
- 只在「輸出會寫回被監看的樹內」（輸出資料夾＝folder，或不在 folder 內以外的情形，`FolderWatcher` 的 :262-264 判斷）時才寫，避免重啟後把昨天的輸出再轉一次。
- 路徑：`<rule.folder>\.filee-watch-<sha256(UTF-8(rule.id)) 前 16 個 hex，小寫>.json`（:18-19）；寫成功後設 Hidden 屬性（:56-69 `Save`），先寫 `<檔>.tmp` 再 `Move(overwrite)`。
- 內容：`System.Text.Json` **預設選項**（PascalCase、**不縮排**）的陣列：`[{"Path":"C:\\in\\conv.jpg","Length":123,"LastWriteTicks":638000000000000000,"IsDirectory":false}, …]`。`LastWriteTicks` 是 `LastWriteTimeUtc.Ticks`（.NET ticks）。讀取（預設選項＝**區分大小寫**）時逐筆驗證：路徑必須在 folder 之內（防止被手改後排除整個 root）、檔案仍存在且 Length/ticks 相同才算數；目錄項目＝「該目錄內的一切都是產物」。
- CLI 的 `watch` 固定 `Id="cli"`（`Cli.cs:92`）→ 檔名 `.filee-watch-99bb88401742848e.json`〔`sha256("cli")` 前 16 hex，我算的〕；兩個 CLI watch 共用同一資料夾會共用同一份狀態檔。

### 4.3 `filee watch` 能否直接代替？—— 不建議

實測：`FILEE_DATA_DIR=<scratch> filee-cli.exe watch <scratch\watch1> --to jpg`（隱藏視窗啟動 4 秒後量測、再丟一個新檔轉換後量測，最後 `Stop-Process`）：

| 狀態 | WorkingSet | PrivateBytes | 備註 |
|---|---|---|---|
| 待機 4 s | **45.2 MB** | 15.4 MB | 11 個執行緒 |
| 轉完 1 個 PNG→JPG 後 | **64.5 MB** | 24.3 MB | peak 64.5 MB |
| 對照：使用者現在執行中的完整 Filee.exe（托盤） | 272.3 MB | 195 MB | 〔實測〕 |

只轉換新檔（`pre.png` 啟動前就在，未轉；`new.png` 轉成 `converted\new.jpg`）；子程序只有 conhost。**N 個 rule＝N 個程序 ≈ N×45–65 MB WorkingSet**；4 條 rule 就已接近常駐 Filee.exe 的 272 MB，失去「輕量外殼」的意義。

其他缺陷（`Cli.cs:84-120`）：
- 只有 `-o`、`-r`、`--move-originals`、`--settle`，**不吃 `include`／`fileNamePattern`／`renames`**，也不經 `WatchRule.Apply`（只做 `UseFolder` + `KeepFoldersFrom = rule.Folder`）。
- **不寫 history**、不帶 `watchRuleId`。
- `Id` 固定 `"cli"`（狀態檔衝突，見上）。
- 輸出走 stdout，印成人類可讀行；程序只能靠 Ctrl+C／結束程序停止。
- 重啟 CLI 程序＝重載整個引擎目錄（冷啟動 ~0.4 s）。

**建議做法**：Rust 自己實作 FolderWatcher（`notify` crate + 同樣的 1 秒 tick、settle 2 s、`share_mode(0)` 開檔測試、`.tmp/.crdownload…` 與 `~$` 排除、`_handled` 去重、`.filee-watch-*.json` 狀態檔格式同上），每個 ready 批次呼叫**一次** `filee-cli convert <files…> --preset <臨時 preset id>`（§3.5(f)），結束後依 §2 寫 history（帶 `watchRuleId`），再自行做 `MoveToOriginals`（規則見上）。`watchFolders` 的 enabled／presetId／folder 變更要在 1.5 秒去抖後重建 watcher（對應 `WatchFolderService.ApplyDelay`，`WatchFolderService.cs:16,29-33`）；watcher 的簽章包含 `folder|presetId|includeSubfolders|outputFolder|originals|include|fileNamePattern|renames`（:101-104）。

重試（上游 Retry for watch folders，`WatchFolderService.cs:91-98`）：Rust 若要支援，等價於對失敗檔重跑 §上面流程，成功者再 `MoveToOriginals`。

---

## 5. SingleInstance、命令列、右鍵選單、開機啟動

### 5.1 SingleInstance（`App/Services/SingleInstance.cs`）

- 名稱：`"Filee-" + 大寫十六進位(SHA256(UTF8(key)))[..12]`（:37-41），`key = Environment.UserName`；設了 `FILEE_DATA_DIR` 時 `key = UserName + "|" + 該路徑去掉結尾分隔符後 ToUpperInvariant()`。
  - 本機 `UserName=User` → **`Filee-B512D97E7CBF`**（我算的 `sha256("User")[:12].upper()`；`[System.IO.Directory]::GetFiles('\\.\pipe\')` 實測管道 `\\.\pipe\Filee-B512D97E7CBF` 存在，吻合）。
  - 若使用者 `alice` 以 `FILEE_DATA_DIR=C:\Users\alice\AppData\Roaming\Filee` 啟動（同一資料夾、但走「獨立資料夾」模式）→ `Filee-A887F1A84BE2`〔計算值：`sha256("alice|C:\USERS\ALICE\APPDATA\ROAMING\FILEE")[:12]`〕，與預設名不同。
- 互斥體：`new Mutex(initiallyOwned:true, @"Local\" + name, out createdNew)`（:47-48）；`createdNew` ＝ first instance。`Local\` 是 session 內，各 Windows session 各一個。
- 管道（server，first instance）：`NamedPipeServerStream(name, PipeDirection.In, maxInstances:1, Byte, Asynchronous | CurrentUserOnly)`，迴圈：等連線 → `JsonSerializer.DeserializeAsync<string[]>` 到 EOF → 觸發 `ArgumentsReceived` → 回圈建下一個 server 實例（:55-95）。`CurrentUserOnly` ＝ 管道 DACL 只給目前使用者 SID；連線被半途中斷（`IOException`/`JsonException`）就繼續服務；`UnauthorizedAccessException` → 等 500 ms 重試。
- **協定**：client `NamedPipeClientStream(".", name, PipeDirection.Out, CurrentUserOnly)` → `Connect(≤1000ms)` →（先呼叫 `Foreground.AllowOtherProcess()`，讓 first instance 取得前景）→ 寫入 **整個 argv（不含 exe）的 JSON 字串陣列，緊湊、無 BOM/無換行**，例 `["--convert","C:\\a.png","C:\\b.png"]` → 關閉連線（:110-141）。管道忙（server maxInstances=1 正在處理上一個）或尚未建立 → 100 ms 重試，總期限 `ForwardTimeout=30 s`（`Program.cs:33`）；連續 5 次 `UnauthorizedAccessException`（對方權限不同，例如以系統管理員身分跑）→ 放棄。
- Rust 要接手 first-instance 角色必須：① 在任何上游 Filee.exe 之前搶到 `Local\Filee-B512D97E7CBF` 互斥體；② 建同名管道 server，DACL 比照目前使用者；③ 解析 JSON 字串陣列。**若上游 Filee.exe 已在跑**（本機目前就是，PID 34612、管道存在），先照安裝程式的做法請它退出：轉發 `["--quit"]` 並等它釋放互斥體（`Program.cs:188-208` `RunInstallerCommand`：`ForwardToFirst(["--quit"], 20s)` → `WaitForFirstToExit`）。
- first instance 收到 args 後：`CommandLine.Parse(args)` → `HandleCommandLine(forwarded:true)`（`App/App.axaml.cs:212-218`、:329-384）。

### 5.2 命令列與 `--convert`／`--convert-list`（`App/Services/CommandLine.cs:1-194`）

| 旗標 | 意義 |
|---|---|
| （無） | 開主視窗 |
| `--background` | 只在托盤（開機自啟用） |
| `--settings` | 開主視窗（與 `--background` 並用也會開） |
| `--whats-new[=<舊版本>]` | 與 `--background` 並用：顯示該版「新功能」 |
| `--convert a b …` | 為這些檔開甜甜圈（右鍵選單／傳送到）。`collecting` 之後的非 `--` 開頭參數、且存在的檔案或資料夾 → `Path.GetFullPath`；不存在的靜默略過（:107-117） |
| `--convert-list <檔>` | 同上，但路徑來自一個 UTF-8 檔（有無 BOM 皆可），一行一個、略過空行、取 `Trim()` 後存在者（:170-181，`ReadListFile`） |
| `--quit` | 要求執行中的 Filee 結束（轉發給 first instance）；無視窗 |
| `--uninstall-cleanup` | 退出後清掉登錄 |
| `--install-engines=a,b`／`--start-with-windows=on\|off`（或 `--start-with-system`）／`--context-menu=on\|off`／`--language=ko` | 安裝程式用 |

- `--convert-list` 讀完後會刪檔，但**只刪**檔名以 `Filee-convert-` 開頭、副檔名 `.txt`、且位於 `%TEMP%` 的檔（`IsOwnListFile`，:154-168；`DeleteListFile` :141-151）。
- first instance 對 `--convert`：`_pendingConvertFiles.AddRange`，**350 ms 去抖**（`App.axaml.cs:355-356`；經典選單 `MultiSelectModel=Player` 會對每個選取檔各啟動一個 Filee.exe），之後 `Distinct(FileSystemPaths.Comparer)`（Windows＝OrdinalIgnoreCase），在 `TriggerService.LastCursor`（沒有則主視窗位置 +200,+200）彈出甜甜圈：`RadialController.ShowForFiles(x, y, files, forAnotherProcess:true)`（:404-411）。
- Rust 對 `--convert` 的等價行為：收齊（去抖 ~350 ms）→ 依 `profiles.json` 選 profile → 在滑鼠位置顯示環形選單。

### 5.3 CLI 子程序與 Filee.exe 的差別

`filee-cli.exe` 與 `Filee.exe` 是兩個獨立 exe（同在 `C:\Program Files\Filee\`）。CLI **不碰**單一執行個體、登錄、托盤。

### 5.4 右鍵選單、傳送到、開機自啟

全部由 `App/Services/SystemIntegrationService.cs:27-84` 在**每次 Filee.exe 啟動／每次 `SaveSettings`** 套用（`ApplySettings`，`App.axaml.cs:270-299`）；設了 `FILEE_DATA_DIR` 時完全跳過（:30-36）；非 Release 版也跳過（:37）。

| 項目 | 確切位置與值 | 出處／本機實測 |
|---|---|---|
| 開機自啟 | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `Filee`，REG_SZ = `"<exe 完整路徑>" --background`；寫完立刻讀回比對，被防毒清掉會丟 `StartupEntryNotKeptException`；`startWithSystem=false` 刪值 | `WinPlat/WindowsPlatformServices.cs:250-263`（常數 :15-17）。**本機實測**：`"C:\Program Files\Filee\Filee.exe" --background` |
| 傳統（Win10／Win11 的「顯示更多選項」）右鍵動詞 | `HKCU\Software\Classes\*\shell\Filee`：`MUIVerb`＝本地化標題、`Icon`＝`"<exe>",0`、`MultiSelectModel`＝`Player`；子鍵 `command` 的預設值＝`"<exe>" --convert "%1"` | `WindowsPlatformServices.cs:282-302,333-343`。**本機實測：此機碼不存在**（因為 Win11 套件選單已啟用，傳統動詞被刻意移除，:293-296 `RemoveClassicVerb`，避免「顯示更多選項」重複） |
| Win11 頂層選單（IExplorerCommand） | sparse MSIX 套件 `Filee.ExplorerMenu`（family `Filee.ExplorerMenu_c73vxh346rtay`，Publisher `CN=Filee, OID.2.25.311729368913984317654407730594956997722=1`，外部位置＝安裝資料夾；COM CLSID `6DB0E670-807F-4D22-BE3F-20492D0F4AB1`；`desktop5:ItemType Type="*"`）；原生 `FileeExplorerMenu.dll` 在 explorer.exe 內執行，**Invoke 時啟動同資料夾的 `Filee.exe`**，命令列 `"<exe>" --convert "<path1>" "<path2>"…`；超過 `MAX_COMMAND_LINE` 改寫 `%TEMP%\Filee-convert-<pid>-<tick>-<n>.txt`（UTF-8、CRLF、一行一個）並呼叫 `--convert-list "<那個檔>"` | `WinPlat/ExplorerMenuPackage.cs:14-144`；`src/Filee.ExplorerMenu/FileeExplorerMenu.c:31-34,103-114,326-385`。**本機實測**：套件已註冊（`Get-AppxPackage Filee*` → `Filee.ExplorerMenu_c73vxh346rtay`） |
| Win11 選單標題／開關 | `%APPDATA%\Filee\explorer-menu.txt`：第一行＝標題（UI 語言），檔案存在＝選單顯示；DLL 每次右鍵讀取（`FILE_SHARE_*` 全開），立即生效、不需系統管理員。內容 UTF-8 **無 BOM**、行尾 `\r\n`，`ReplaceLineEndings(" ").Trim()+"\r\n"` | `WinPlat/ExplorerMenuRegistration.cs:36-37,88-118`；**本機實測**：`用 Filee 轉換\r\n`（18 bytes） |
| 傳送到（SendTo） | `%APPDATA%\Microsoft\Windows\SendTo\Filee.lnk`，Target＝`<exe>`、Arguments＝`--convert`、Icon `<exe>,0`；用 `WScript.Shell` 建立 | `WindowsPlatformServices.cs:282-302,425-446`。**本機實測**：Target=`C:\Program Files\Filee\Filee.exe`、Arguments=`--convert` |

Rust 版的含意：
1. **Win11 套件註冊需要系統管理員**（`ExplorerMenuPackage.cs:31-36` 註解、`ExplorerMenuRegistration.AddAsync`），且 DLL 寫死叫 `Filee.exe` —— Rust 外殼不可能取代這個入口，只能「讓 Filee.exe 當轉發器」（Rust 持有管道時，Filee.exe 只轉發 argv 後以 0 退出，`Program.cs:81-93`）。選單標題檔是 Rust 可以直接寫的（要關閉選單＝刪檔）。
2. 傳送到、經典動詞、Run 機碼的目標是 `Filee.exe`；若 Rust 想改成自己的 exe：**防毒警告**——未簽章自編 exe 寫 `HKCU\...\Run` 會被 Bitdefender 擋（使用者 CLAUDE.md 記載 2026-09-28、2026-10-26 兩次事故），**不要自動寫**，改為文件化讓使用者自己設；本規格不建議動這幾個機碼。
3. 若 Rust 與上游並存，**開機只能有一個 first instance**：Run 機碼目前啟動 `Filee.exe --background`（會帶起整個 .NET 常駐＋全域 hook）。要用 Rust 當常駐，必須由使用者把 Run 機碼改指向 Rust（或關掉 `startWithSystem`）。

### 5.5 更新檢查（UpdateService，只需知道怎麼查）

`App/Services/UpdateService.cs:40,224-262`：`GET https://api.github.com/repos/KnifeLemon/Filee/releases/latest`（`RepositoryUrl` 預設 `https://github.com/KnifeLemon/Filee`，:77），`User-Agent: Filee/<版本>`；讀 `tag_name`，與目前版本比較（`IsNewer`，:1177）；啟動後 20 秒首次檢查，之後每 **6 小時**（`CheckInterval`，:40；`StartPeriodicChecks(..., 20s)` 在 `App.axaml.cs:WatchForUpdates`）。Release 資產需有 `*-win-Setup.exe` 與一個 sha256 清單（`FindFiles`，:681-710）；安裝版（有 `uninstall.exe`）才會自動更新（`IsInstalled`，:97-98）。`settings.checkForUpdates=false` 關閉，`notifiedUpdateVersion` 記錄已通知版本（避免每次都跳）。Rust 外殼若要提示新版，只需照此查 GitHub 並尊重這兩個設定；實際升級仍交給上游安裝程式。

---

## 6. 出處對照（行號速查）

| 主題 | 檔案:行 |
|---|---|
| 資料夾、檔案路徑、Load、Save、AddHistory、Write、Read、EnsureConsistency | `Core/Settings/UserDataStore.cs`:33,42,64-67,70-91,93-161,272-302,304-337 |
| 序列化選項 | `Core/Settings/FileeJsonContext.cs`:10-24 |
| AppSettings／Donut／Theme／DefaultOutput／BrowserDrops／WithDefaultOutput | `Core/Settings/AppSettings.cs`:36-120,123-297,276-296,329-347 |
| TriggerGesture | `Core/Settings/TriggerGesture.cs`:7-88 |
| Preset／lenient enum | `Core/Presets/Preset.cs`:9-81,83-285 |
| OutputRule／RenameStep | `Core/Presets/OutputRule.cs`:6-197 |
| OutputPathResolver | `Core/Presets/OutputPathResolver.cs`:24-65,71-79,120-151 |
| AskedLocation（每次詢問） | `Core/Presets/AskedLocation.cs`:12-80 |
| ToolbarProfile／ProfileSelector | `Core/Profiles/ToolbarProfile.cs`:10-105 |
| HistoryEntry／Output／Failure | `Core/History/HistoryEntry.cs`:9-139 |
| WatchResults | `Core/History/WatchResults.cs`:1-38 |
| WatchRule／FileNameFilter／FolderWatcher／GeneratedWatchOutputs | `Core/Watching/*.cs`（本文件內標示） |
| ConversionJob／FileResult／JobState | `Core/Conversion/ConversionJob.cs`:7-168 |
| JobQueue | `Core/Conversion/JobQueue.cs`:59-160,1300-1308 |
| CLI | `Cli/Program.cs`:1-16；`Cli/CliHost.cs`:25,28-43,46-50；`Cli/CliOptions.cs`:27,29-133；`Cli/Cli.cs`:17-20,53-82（convert）、84-120（watch）、123-151（RunJobAsync）、153-165（Describe）、176-181（Shorten）、182-220（WriteJson）、223-280（ResolvePreset）、282-286（UseFolder）、299-323（ExpandInputs）、360-368（presets） |
| ConversionService | `App/Services/ConversionService.cs`:69-92,105-123,156-161,179-197,206-242 |
| WatchFolderService | `App/Services/WatchFolderService.cs`:16-34,45-85,91-104 |
| SingleInstance／Program | `App/Services/SingleInstance.cs`:14,37-52,55-95,110-141；`App/Program.cs`:48-122,188-208 |
| CommandLine／HandleCommandLine | `App/Services/CommandLine.cs`:1-194；`App/App.axaml.cs`:212-218,270-299,329-411 |
| SystemIntegration／Run／選單／SendTo | `App/Services/SystemIntegrationService.cs`:27-84；`WinPlat/WindowsPlatformServices.cs`:250-302,333-343,425-446 |
| Explorer 套件／DLL／標題檔 | `WinPlat/ExplorerMenuPackage.cs`:14-144；`WinPlat/ExplorerMenuRegistration.cs`:36-118；`Filee.ExplorerMenu/FileeExplorerMenu.c`:31-34,103-114,326-385 |
| 更新 | `App/Services/UpdateService.cs`:40,77,97-98,224-262,681-710,1177 |
| 引擎根目錄（FILEE_DATA_DIR 副作用） | `Filee.Engines/Infrastructure/EngineEnvironment.cs`:14-47 |

---

## 7. 移植風險

### 最大的 3 個
1. **Filee.exe 整檔覆寫、不重讀（遺失更新）。** Rust 寫的 settings/history 會在 Filee.exe 下一次存檔被它的記憶體版本蓋掉；Filee.exe 又常在不經意時存檔（關視窗、縮放、計成功數、更新通知）。→ Rust 擁有的欄位要極少（`paused`、`watchFolders`…），設定視窗開著時 Rust **暫停寫入**並延後 history，視窗關閉（子程序結束）後重讀；歷史採 read-modify-write + 原子替換。**Rust 寫壞任何一個非 lenient enum 或欄位名大小寫，整檔被當損毀→上游回預設並覆蓋使用者庫**，所以只能改 `serde_json::Value` 並嚴格寫出上游認得的值。
2. **誰是 first instance、Filee.exe 的登錄寫入會搶 Rust。** Filee.exe 每次啟動都會（非 `FILEE_DATA_DIR` 模式）把 Run 機碼與傳統右鍵動詞／SendTo 重寫成指向 `Filee.exe`（`SystemIntegrationService.Apply`）；而 Rust 臨時啟動 Filee.exe 當設定頁時，若 Rust 已持有 `Local\Filee-B512D97E7CBF`，那個 Filee.exe 會把自己的 argv（含 `--settings`）**轉發給 Rust 並立即結束**，設定頁根本開不起來。→ 建議以環境變數啟動設定頁：`FILEE_DATA_DIR=%APPDATA%\Filee`（同一資料夾！）。它讓 Filee.exe 使用另一個 single-instance 名稱（`Filee-4E4906710B69`，§5.1）且**完全不碰系統整合**（`SystemIntegrationService.cs:30-36`），資料仍讀寫同一組 JSON。**副作用（必須處理）**：① 下載式引擎根目錄變成 `%APPDATA%\Filee\engines`（`EngineEnvironment.cs:25-30`）→ 設定頁「引擎」分頁看不到 `%LOCALAPPDATA%\Filee\engines` 內已裝的（本機有 `ocr-languages`），需要 junction（建立前依防毒規則徵得同意）；② 這個臨時 Filee.exe 仍會啟動全域滑鼠 hook（`TriggerService`），與 Rust 的觸發重複彈出甜甜圈 → Rust 在子程序存活期間暫停自己的觸發；③〔**未實測**：我沒有啟動 GUI 版本驗證；此條建議是由原始碼推得，信心：中〕。
3. **進度與取消能力有限。** CLI 沒有進度事件，只有「每檔完成一行」；取消只能 Ctrl+C／Break（未實測）或殺程序（殺程序留半成品與 `%TEMP%\Filee\<jobId>`）。介面設計要以「檔案數進度＋不定進度」為準，不要承諾單檔百分比。

### 其他風險與建議

4. **history 由 Rust 負責**：CLI 不寫；若漏寫，上游「Recent conversions」永遠是空的，Undo/Retry 也不會有。`presetName` 要用 UI 語言（CLI JSON 的 `"preset"` 是英文）。見 §2。
5. **`{preset}` token 在 CLI 是英文名稱**（`CliHost.cs:25`），在 App 是 UI 語言；同一個 preset 的輸出檔名可能因入口不同而不同（只有 pattern 含 `{preset}` 時才發生）。
6. **自動轉換資料夾不要用 `filee watch`**（§4.3）：記憶體 45–65 MB/rule、缺 include/pattern/renames、不寫 history。用自製 watcher + 臨時 preset（§3.5(f)）；臨時 preset 需要臨時資料夾（含 `settings.json` 副本），並有 `FILEE_DATA_DIR` 的 engines 目錄副作用（同上第 2 點）。
7. **每次詢問存檔位置（`askEveryTime`）**：CLI 忽略它（用 `location`）。Rust 若要支援，自己彈對話框，資料夾模式用 `-o <folder>`；「指定單一檔名」模式要用臨時 preset（`fileNamePattern=選的檔名（不含副檔名）`、`customFolder`、`renames=[]`、`conflict` 視需要），上游的 `ConfirmedPath` 精確覆蓋語意無法重現（它是 `[JsonIgnore]`）。
8. **Win11 頂層右鍵選單由上游 DLL 啟動 `Filee.exe`**：Filee.exe 必須保留安裝。Rust 持有管道時每次右鍵都會多一個 Filee.exe（.NET 冷啟動）只為了轉發；延遲約為 .NET 冷啟動時間〔未量測〕。選單本身的開關可由 Rust 直接寫／刪 `explorer-menu.txt`。
9. **管道安全**：上游用 `CurrentUserOnly`。Rust 的 server 也要限制到目前使用者，否則同機其他使用者可以對你的甜甜圈注入任意檔案清單。另外 `--convert-list` 只刪 `%TEMP%\Filee-convert-*.txt`，Rust 照做，不要刪別處的檔。
10. **Windows 路徑比較不分大小寫**（`FileSystemPaths.Comparer`＝`OrdinalIgnoreCase`）：去重、`IsInside`、`_handled` key 都要照做。
11. **schema 漂移**：`schemaVersion` 現在是 54，上游每次改版可能 +1 並新增欄位。Rust 的讀取層要容忍未知欄位、缺欄位用上游的預設值（尤其 `startWithSystem` 預設 true、`keepHistory` 預設 true、`historyLimit` 預設 200），寫入層永遠保留未知欄位。Preset 內部選項類別不要建 struct，原樣傳遞。
12. **JSON 讀寫細節**：`modifiers` 是 flags 字串（`"Ctrl, Alt"`）；時間欄位要精確到 100ns（`writtenUtc` 逐字比對）；`.filee-watch-*.json` 是**PascalCase**、不縮排，與其他四檔的 camelCase 不同。
13. **Bitdefender**：不要自動建 Run 機碼／捷徑／排程；不要把 Rust exe 複製到 AppData 再啟動；臨時資料夾放 `%TEMP%` 下沒事（只放 JSON）；junction 屬於「改系統結構」，先問使用者。

---

## 附：我實際跑過的指令與結果摘要

環境：`C:\Program Files\Filee\filee-cli.exe`（`filee version` → `filee 2.2.0`）；測試檔與資料夾都在 `…\scratchpad\agent-data\`（`in\a.png b.png`、`big\p0-5.png`、`data\`＝`%APPDATA%\Filee` 四個 JSON 的複本、`wf\`、`watch1\`…）。全程用 `FILEE_DATA_DIR` 指向 `agent-data\data`（或 `data2`），**未寫入 `%APPDATA%\Filee`**；未改登錄；未編譯任何 C#。

| 指令 | 結果 |
|---|---|
| `filee-cli version` / `help` / `presets` | `filee 2.2.0`；help 全文；presets 表（`ID NAME TARGET`） |
| `convert in\a.png in\b.png --preset to-jpg --json` | exit 0；JSON 見 §3.5(a) |
| `convert … --preset to-webp -o out1` | exit 0；人類模式兩行 `ok …` + `Converted 2 of 2 file(s).` |
| `convert in\a.png in\bad.png --preset to-jpg --skip --json` | exit 1；`skipped:1, failed:1`；0.40 s |
| `convert in\bad.png --to jpg` | exit 1；`fail …`、`Converted 0 of 1 file(s), 1 failed.` |
| `convert in\a.png in\b.png --preset merge-pdf -o out2 --json` | exit 0；輸出 `out2\a_merged.pdf` 掛第一檔 |
| `convert in\a.png in\nonexist.png --to jpg --json` | exit 2；stderr `filee: not found: …`；stdout 空 |
| `convert empty --to jpg` | exit 3；`filee: no files to convert.` |
| `convert … --preset rotate` / `--preset tool-crop` | exit 2；group／tool 訊息 |
| `convert big\p0..p5.png --preset to-avif -o out3 --overwrite`（PowerShell 逐行加時間） | 6 行 ok 於 1.63–1.86 s 陸續出現（順序＝完成順序），結束行 1.86 s |
| `convert … --quiet` | 無輸出、exit 0 |
| 臨時 preset（`FILEE_DATA_DIR=data2`）`convert wf\top.png wf\sub\deep.png --preset eph-1` | `wfout\sub\DEEP_X.jpg`、`wfout\TOP_X.jpg` |
| `watch watch1 --to jpg`（隱藏、重新導向輸出）量 Working Set | 待機 45.2 MB／Private 15.4 MB／11 執行緒；轉 1 檔後 64.5 MB／24.3 MB；`pre.png` 未轉、`new.png`→`converted\new.jpg`；之後 `Stop-Process` |
| `Get-Process Filee`（使用者執行中的 GUI） | 272.3 MB WS／195 MB Private |
| `Get-ItemProperty HKCU:\...\Run`、`...\Classes\*\shell\Filee`、`Get-AppxPackage Filee*`、SendTo `.lnk`、`\\.\pipe\` 列舉 | Run＝`"C:\Program Files\Filee\Filee.exe" --background`；傳統動詞**不存在**；套件已註冊；`.lnk` → `Filee.exe --convert`；管道 `Filee-B512D97E7CBF` 存在 |
| Python `hashlib` | `sha256("User")[:12]`＝`B512D97E7CBF`；`sha256("alice|C:\USERS\ALICE\APPDATA\ROAMING\FILEE")[:12]`＝`A887F1A84BE2`；`sha256("cli")[:16]`＝`99bb88401742848e` |

**沒跑成／未驗證**：
- `Ctrl+Break` 優雅取消 CLI（需要 P/Invoke `GenerateConsoleCtrlEvent`；本機規則禁止 `Add-Type` 編譯，沒做）。
- 啟動 GUI 版 Filee.exe 驗證 `FILEE_DATA_DIR=%APPDATA%\Filee` 的「獨立 instance、不寫登錄」行為（會有 hook 與寫入使用者資料夾的風險）；只由原始碼推得。
- `history.json` 真實內容樣本（本機是 `[]`，也不能為了取樣去觸發 App 轉檔）；`finishedAt` 格式為依 .NET 行為推論。
- .NET 讀 UTF-8 BOM 的行為（沒測）。
- Rust 當 first instance 時 Win11 DLL 啟動的 `Filee.exe` 轉發延遲（沒量）。
