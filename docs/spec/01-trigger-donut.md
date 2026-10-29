# 01 觸發與環形選單（donut）移植規格

來源：Filee v2.2.0（MIT），本機 `upstream/`。目標：Rust、Windows only。
本文件讓 Rust 實作者不必再讀 C# 就能做出行為一致的版本。每一點後面的 `〔檔:行〕` 是上游出處。

路徑縮寫（皆在 `upstream/src/` 底下）：

| 縮寫 | 路徑 |
|---|---|
| `App/` | `Filee.App/` |
| `Core/` | `Filee.Core/` |
| `Win/` | `Filee.Platform.Windows/` |
| `Trig/` | `Filee.App/Services/Triggers/` |
| `RC` | `Filee.App/Services/RadialController.cs` |
| `DM` | `Filee.App/Controls/DonutMenu.cs`（另有 `.Groups.cs`、`.Flip.cs`、`.Progress.cs`） |
| `DG` | `Filee.App/Controls/DonutGeometry.cs` |

座標單位：全域 hook 給的是**實體像素**（螢幕座標）；donut 內部繪製用**邏輯像素（DIP）**，視窗位置再乘上螢幕縮放（`RC:583-598`）。Filee 本身是 PerMonitorV2 DPI aware（`App/app.manifest:27-28`）。

---

## 0. 總覽：一次轉換的觸發鏈

```
全域 hook ─► GestureDetector（純狀態機）─► TriggerService（丟到 UI thread）─► RadialController ─► RadialWindow(DonutMenu)
```

- 三種啟動來源：(a) 拖曳手勢 `ShowForDrag`（尚不知道檔案）；(b) 按住／快捷鍵 `SelectionGesture` → 讀檔案總管選取 → `ShowForFiles`（click mode）；(c) 右鍵選單／Send To／首頁拖放區／再次啟動帶檔案 → 也是 `ShowForFiles`（`App/App.axaml.cs:92-94, 369`、`App/Views/Pages/HomePage.axaml.cs:61, 76`）。(c) 不屬於本份觸發規格，但共用同一個 donut。
- 事件進 UI thread：`DragStarted`／`DragEnded`／`SelectionGesture` 皆 `Dispatcher.UIThread.Post`（`Trig/TriggerService.cs:52-55, 350`）。

---

## 1. 觸發：設定欄位與每種 TriggerKind 的狀態機

### 1.1 設定欄位（`TriggerGesture`，`Core/Settings/TriggerGesture.cs:53-75`）

| 欄位 | 型別／預設 | 說明 |
|---|---|---|
| `Id` | string，`Guid.N` 前 8 碼 | |
| `Enabled` | bool，true | 關閉的手勢不進偵測器（`GestureDetector.cs:52`） |
| `Kind` | `Drag`（預設）／`Hold`／`KeyChord` | |
| `Modifiers` | flags：`Ctrl=1 Alt=2 Shift=4 Meta=8`；預設 `Ctrl`；左右鍵視為相同 | Meta = Windows 鍵 |
| `Button` | `None/Left/Right/Middle/X1/X2`；預設 `Left`；KeyChord 存檔時強制 `None`（`App/ViewModels/Pages/TriggersPageViewModel.cs:104`） | |
| `Key` | string?，SharpHook KeyCode 名稱去掉 `Vc`（`"Space"`、`"F2"`、`"Q"`） | **只有 KeyChord 使用**；Drag／Hold 即使有填也被偵測器忽略（`GestureDetector.cs` 全檔只在 `KeyDown` 讀 `Key`，註解「optional otherwise」與實作不符） |
| `DragThreshold` | int，0 = 系統預設；UI 夾在 0–200（`TriggersPageViewModel.cs:106`） | UI 只對 Drag 開放設定 |
| `HoldMilliseconds` | int，預設 450；UI 夾在 150–3000（`:107`） | 只對 Hold 有意義 |
| `Scope` | `FileManager`（預設）／`Anywhere` | |

預設手勢（Windows）：`Drag + Ctrl + Left`；`KeyChord + Ctrl+Alt + Space`（Button=None）〔`TriggerGesture.cs:78-87`〕。
「新增手勢」UI 的預設值是 `Alt + Left`，Kind=Drag（`TriggersPageViewModel.cs:334-340`）。

AppSettings 相關欄位〔`Core/Settings/AppSettings.cs`〕：`Paused`（:182）、`Donut`（:186）、`Triggers`（:188，預設 `TriggerGesture.Defaults()`）、`ExcludedProcesses`（:191，不含 `.exe` 的程序名清單）。

### 1.2 全域輸入來源與執行緒模型

- Filee 用 SharpHook（libuiohook）。Windows 上等同 `WH_MOUSE_LL` + `WH_KEYBOARD_LL`。**Hook 只監聽，不吞事件**（`Trig/TriggerService.cs:80-91` 註解）。
- Hook 的 OS callback 只做「放進佇列」，由另一條背景執行緒（`"Filee input events"`）取出後才派發，因為 Windows 會等 callback 回傳才繼續送滑鼠（`Trig/QueuedGlobalHook.cs:24-42`）。Hook 執行緒優先權 `Highest`（`TriggerService.cs:230-231`）。**scope 判斷（WindowFromPoint、取程序名）因此是在事件執行緒上做，不在 OS hook callback 上。**
- Rust 實作對應：hook callback 內只 `try_send` 到 channel；另一條執行緒跑 GestureDetector。hook 若 callback 太慢，Windows 會靜默移除 hook（註解稱約 1 秒，`TriggerService.cs:1-3`）。
- 事件映射：
  - 滑鼠鍵 `Button1..5` → `Left, Right, Middle, X1, X2`（`:352-360`）。
  - 修飾鍵從事件 mask 取：Ctrl/Alt/Shift/Meta（左右皆算）（`:362-370`）。Rust 需自行維護修飾鍵狀態（用 `GetAsyncKeyState` 或在 keyboard hook 追蹤）。
  - 移動事件帶「目前按住的按鍵」：mask 內 `Button1→Left … Button5→X2`，優先序 Left>Right>Middle>X1>X2（`:372-378`）。
  - `MouseDragged` 與 `MouseMoved` 兩種事件都走同一個 `OnMove`（`:201-202`）。
  - 鍵名＝KeyCode 去掉 `Vc`（`:381-385`）。常用對照見 `App/Services/KeyNames.cs:10-45`（A–Z、`0`–`9`、`NumPad0..9`、`F1..F24`、`Space/Enter/Tab/Escape/Backspace/Insert/Delete/Home/End/PageUp/PageDown/方向鍵`、`BackQuote/Minus/Equals/OpenBracket/CloseBracket/Backslash/Semicolon/Quote/Comma/Period/Slash/PrintScreen/Pause`）。
- `LastCursor`（實體像素）只在移動事件時更新（`:321-322`）；按下／放開事件的座標由 `ButtonPoint` 取（Windows 直接用事件座標，`:312-313`）。
- 系統拖曳門檻 `SystemDragThreshold = max(2, GetSystemMetrics(SM_CXDRAG))`（`Win/WindowsPlatformServices.cs:245`；`SM_CXDRAG=68`，`Win/NativeMethods.cs:9`）。一般為 4。

### 1.3 GestureDetector 狀態（`Trig/GestureDetector.cs`）

狀態變數：`_armed`（Armed? = {Drag 手勢?, Hold 手勢?, X, Y, Token}）、`_dragActive`、`_lateRejected`、`_lastX/_lastY`，皆以鎖保護（:18-23）。`Paused` 為 true 時一切不偵測（:38）。

**共用規則**
- 修飾鍵比對為**完全相等**：`g.Modifiers == modifiers`（:87, :139, :197）。多按任何一個修飾鍵都不匹配。
- 手勢清單中同種類的第一個符合者勝出（`FirstOrDefault`，順序＝設定清單順序）。
- 門檻判斷 `Moved`：`|x − 起點x| > t || |y − 起點y| > t`，**嚴格大於、逐軸比較（不是歐氏距離）**；`t = gesture.DragThreshold > 0 ? 它 : SystemDragThreshold`（:123-127）。單位為實體像素。Hold 手勢也走這個函式，它的 `DragThreshold` 通常是 0，所以用系統門檻。

#### Drag（修飾鍵 + 按住滑鼠鍵移動超過門檻）

1. `MouseDown(button, x, y, modifiers)`（:57-83）：
   - 記 `_lastX/Y`；`_armed=null`；`_lateRejected=false`；Paused 則 return。
   - `drag = FirstInScope(Drag, button, modifiers, x, y)`；`hold = FirstInScope(Hold, …)`。`FirstInScope` 要求 `Kind、Button、Modifiers` 全等且 `ScopeFilter(g,x,y)` 為真（:86-87）。
   - 兩者都沒有 → 不 arm。否則 `_armed = Armed(drag, hold, x, y, token)`；`token = TickCount64 ^ (x*7919) ^ y`。
   - 若有 hold：觸發 `HoldTimerRequested(token, hold.HoldMilliseconds)`（外層啟動計時器，見 Hold）。
2. `MouseMove`（:90-104）：更新 `_lastX/Y`；若 `_armed` 為空走「晚到修飾鍵」(3)，否則走 `DetectDragPastThreshold`。回傳手勢不為空時設 `_dragActive=true` 並觸發 `DragStarted(gesture, x, y)`（x,y 是**這次移動事件的位置**，不是按下位置）。
3. `DetectDragPastThreshold`（:109-121）：先看 Hold 是否已移動超過其門檻，是則 `armed.Hold=null`（取消 Hold）；再看 Drag 是否移動超過其門檻，是則 `_armed=null` 並回傳該 drag 手勢。若 Drag 與 Hold 都已空 → `_armed=null`。
4. 晚到修飾鍵 `DetectLateModifier`（:133-146）：「先開始拖、再按 Ctrl」的情境（Explorer 常見）。條件：`!_dragActive && !_lateRejected && !Paused && heldButton != None && modifiers != None`，找 `Kind==Drag && Button==heldButton && Modifiers==modifiers` 的手勢（**不看 MouseDown 時有沒有 arm**）；找到就評估 `ScopeFilter(late, x, y)`：真→回傳（立刻 DragStarted）；假→`_lateRejected=true`（本次按壓不再嘗試）。
   - 注意：這也代表「按著滑鼠鍵並帶著完整修飾鍵、從 scope 外移進 Explorer」會在進入的第一個移動事件觸發。
5. `MouseUp(button, x, y)`（:148-162）：**任何滑鼠鍵放開**都會（不比對 button）清 `_armed`、`_lateRejected`；若 `_dragActive` 則清除並觸發 `DragEnded(x, y)`。
6. 取消條件總結：放開按鍵（→ DragEnded）、`Paused`、Hold 計時到之前移動超門檻（只取消 Hold）、設定改變（`Configure` 會 `_armed=null`，:55）。沒有 Esc 取消；沒有逾時（逾時在 RadialController，見 §2.2）。

#### Hold（修飾鍵 + 按住滑鼠鍵不動 HoldMilliseconds）

- 同一個 `MouseDown` arm（上節 1）。外層（`TriggerService.cs:56-57`）`Task.Delay(ms)` 後呼叫 `HoldElapsed(token)`。
- `HoldElapsed`（:165-181）：僅當 `_armed.Hold != null` 且 `Token` 相同才觸發：`fire=hold`；位置＝**按下時的位置** `(armed.X, armed.Y)`；`_armed=null`；`_lateRejected=true`（Hold 觸發後繼續移動不會再觸發同修飾鍵的 Drag）。觸發 `HoldTriggered`。
- 取消：移動超過門檻（3. 的 `armed.Hold=null`）；`MouseDown`／`MouseUp` 任一次（`_armed` 被清，token 就對不上）；Paused 不會取消已排程的計時器，但 `HoldElapsed` 不檢查 Paused，由 App 端攔（見 §3.1）。
- Drag 與 Hold 可共用同按鍵與修飾鍵（#450）：同時 arm，移動超門檻→Drag，靜止滿時間→Hold（`GestureDetector.cs:7`、:70）。

#### KeyChord（修飾鍵 + Key）

- `KeyDown(key, modifiers)`（:184-202）：Paused 則 return；位置＝`_lastX/_lastY`（最後一次滑鼠事件，**若從未有過滑鼠事件為 (0,0)**）；找 `Kind==KeyChord && Key 與 key 不分大小寫相等 && Modifiers 完全相等 && ScopeFilter` 的第一個，觸發 `KeyChordTriggered(g, x, y)`。
- **沒有鍵盤自動重複過濾**：按住不放會連續觸發（App 端 `_selectionPending` 防重入，見 §3.1）。Flip 鍵才有重複過濾（`TriggerService.cs:94-100`）。

### 1.4 Scope 判斷（`TriggerService.InScope`，`Trig/TriggerService.cs:335-348`）

輸入：手勢、(x, y)。步驟：

1. 若 `gesture.Kind == KeyChord`：取**前景視窗**資訊 `front = GetForegroundWindow()`；否則 `front = null`。
2. `process = front?.ProcessName ?? ProcessNameAt(x, y)`。
3. 若 `process != null` 且（等於 Filee 自己的程序名，不分大小寫，**或**在 `excluded` 集合）→ **false**。
   - Filee 自己的視窗永遠排除（包含 donut 視窗與設定視窗）。
   - `excluded` 由 `Apply` 建立：每筆 `Trim()` 再 `Replace(".exe","")`（`OrdinalIgnoreCase`，是**全字串 Replace，不只結尾**），HashSet 不分大小寫（`:141-142`）。
4. 回傳 `Scope == Anywhere || (front?.IsFileManager ?? IsFileManagerAt(x,y)) || (Kind == Drag && WebBrowsers.IsBrowser(process))`。
   - 即：FileManager scope 下，**只有 Drag 手勢**可以在瀏覽器上觸發；Hold／KeyChord 不行（它們要讀檔案總管選取，`Core/Web/WebBrowsers.cs:1-4`）。

Windows 的實作細節（`Win/WindowsPlatformServices.cs`）：

- **滑鼠手勢（Drag／Hold）的「游標下視窗」**：`RootWindowAt(x,y)` = `WindowFromPoint(POINT{x,y})` → `GetAncestor(hwnd, GA_ROOT)`（:406-410）。
- **桌面圖示層**：`IsFileManagerAt` 先過 `DesktopOrSelf(root)`（:55-63, 99-105）：取 `GetAncestor(root, GA_ROOTOWNER)`，若 owner 是另一個視窗且其類別為 `Progman` 或 `WorkerW`，就把該視窗當桌面（給「用自繪圖示取代 Explorer 桌面」的程式用）。判斷用 `StandsForDesktop`（:95-96）。注意：**程序名（ProcessNameAt）仍是該圖示層自己的**，所以那層程序可被 `excludedProcesses` 排除。
- **是否為檔案總管**（`IsFileManager(windowClass, processName)`，:66-68）：視窗類別屬於 `{CabinetWClass, ExploreWClass, Progman, WorkerW}`（:20-26，`Ordinal` 比對）**或** 程序名屬於 `{explorer, Files, dopus, TOTALCMD64, TOTALCMD, XYplorer, OneCommander, Q-Dir, FreeCommander, FPilot}`（:32-44，不分大小寫）。
- **取程序名**：`GetWindowThreadProcessId(hwnd)` → `Process.GetProcessById(pid).ProcessName`（不含 `.exe`）（:412-430）。失敗回 null。
- **前景視窗**（KeyChord 用）：`GetForegroundWindow()` → `GetAncestor(…, GA_ROOT)`，空則用原 hwnd（:236-243）；`ForegroundWindowInfo(ProcessName, IsFileManager)`（:76-83）。
- **瀏覽器程序名**（`WebBrowsers.IsBrowser`，去掉結尾 `.exe` 後不分大小寫比對；`Core/Web/WebBrowsers.cs:10-27`）：Windows 部分為 `chrome, msedge, firefox, brave, opera, vivaldi, whale, arc, zen, librewolf, waterfox, floorp, chromium, yandex`。（Linux／macOS 名稱略。）

`paused`：
- 設定欄位 `Paused`；托盤選單「Pause／Resume」切換並存檔（`App/Services/TrayService.cs:24-28`）。`TriggerService.Apply` 把它設到偵測器（:138）；偵測器 `MouseDown`／`KeyDown`／晚到修飾鍵都檢查（:67, 135, 190）。
- 設定改變時 `Apply` 被呼叫（`App/App.axaml.cs:284`）；它會重設偵測器的 armed 狀態。

---

## 2. 拖曳模式（Drag）：donut 視窗與 OLE 拖放

### 2.1 視窗規格（`App/Views/RadialWindow.axaml:1-66`，行號以該檔自身計）

- 無邊框、透明、置頂、不顯示於工作列、**不啟動（ShowActivated=False）**、不可縮放、尺寸隨內容：`WindowDecorations=None, Background=Transparent, TransparencyLevelHint=Transparent, Topmost=True, ShowInTaskbar=False, ShowActivated=False, CanResize=False, SizeToContent=WidthAndHeight`（:11-20）。
- 視窗**只建立一次、之後 Show/Hide 重用**（`RC:215-220`）。
- 根 Panel `Root`：`AllowDrop=True`，背景綁 `HitBackground`（:41）。DragEnter／DragOver／DragLeave／Drop 四個處理器掛在 `Root`（`RadialWindow.axaml.cs:20-23`）。
- **關鍵細節：Windows 會讓拖放與滑鼠穿過「完全透明」的像素。** 因此：
  - 拖曳模式等待檔案時，`HitBackground = "#01000000"`（alpha=1/255，幾乎不可見），好讓 Windows 仍把拖放送給此視窗；`DonutOpacity=0`（donut 不畫）〔`App/ViewModels/RadialViewModel.cs:127-140`〕。
  - 檔案進來（`Load`）後 `HitBackground="Transparent"`、`DonutOpacity=1`（:146-152）。此時 donut 只有繪製的像素接得到滑鼠／拖放；群組環開啟時額外畫 alpha=1 的 `HitBackdrop` 補洞（見 §4.8）。
- 每次顯示都 `SetWindowPos(HWND_TOPMOST, 0,0,0,0, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE|SWP_NOOWNERZORDER)` 把視窗推回最上（`Win/WindowsPlatformServices.cs:46-51`、`RC:596-605`）。原因：視窗已標 topmost 時再 Show 不會改 z-order，Windows 可能在別的 app（例如「相片」）開啟後把它壓到一般視窗之下，但仍回報 topmost；結果 Explorer 在 donut 之上，drop 到不了。
- 視窗位置＝以游標為中心、邊長 `sidePx` 的正方形，夾在 `screen.WorkingArea` 內（`WindowPosition`，`RC:573-581`）：
  - `side = SideFor(OuterRadius, ReserveSubRing)`；`sidePx = ceil(side * screenScaling)`（`RC:588-591`）；Windows 的 scaling 取游標所在螢幕的 `Scaling`（macOS 例外用 1，`RC:587`）。
  - `position = (cursor.x − sidePx/2, cursor.y − sidePx/2)`（整數除法）；`x ∈ [area.X, max(area.X, area.Right − sidePx)]`，`y` 同理（夾進工作區，不含工作列）。
  - 顯示前後各設一次 `Position`（顯示可能改變 DPI）。顯示前呼叫 `Motion.Refresh()`（`RC:594-598`）。
- `SideFor(r, subRing) = 2 * (r + PopOut(7) + 14 + (subRing ? SubRingSpace : 0))`；`SubRingSpace = SubGap(6) + SubBand(46) + 10 = 62`〔`DonutMenu.Groups.cs:22-32`〕。預設 r=150：無群組 342、有群組 466（邏輯像素）。
- `ReserveSubRing`：拖曳模式（檔案尚未知）＝任一 profile（含背面）有群組 slice（`RadialViewModel.cs:132`）；click mode＝這個 donut 的任一面有群組（`FitToThisDonut`，:299）。

### 2.2 拖曳模式流程（`RC:84-163`）

**A. DragStarted（手勢成立，檔案未知）→ `ShowForDrag(x, y)`**（`RC:84-106`）

1. `ReleaseProgressToToast()`（上一次的進度環移交給 toast）；`_grace.Cancel()`；`ReplaceWeb(null)`。
2. `EnsureWindow()`；`DonutControl.LeaveProgressMode()`；`_dropped=false; _dragReachedDonut=false; _converted=false`。
3. `viewModel.ResetForDrag()`（見 §2.1：不可見但接得到拖放）；`AllowContextEdit=false`。
4. `PlaceAndShow(window, x, y, activate:false)`（x,y＝手勢成立時的游標位置）。
5. `WatchFlipKey()`（此時 `FlipsFromHook` 為 false，因為還沒檔案）。
6. 安全逾時：`EmptyDragLimit = 10 秒`。若 10 秒後仍是同一次拖曳（`_dragGeneration` 相同）、視窗可見、沒有檔案、非 click mode → `Close(animate:false)`（`RC:73, 100-105`）。用意：萬一錯過按鍵放開（hook 被系統拔掉、UAC 抓走桌面），隱形視窗會擋點擊。

**B. 檔案進入（DragEnter）**（`RadialWindow.axaml.cs:55-73`）

1. 觸發 `DragEntered`（記 `_dragReachedDonut=true`）。
2. `DraggedFiles.Read(e.DataTransfer, forDrop:false)`（先試瀏覽器，見 §2.4，再試本機檔案）；立即 `dragged.Web?.Dispose()`（DragEnter 只要名字，真正的內容在 Drop 時再讀）。
3. `_dragOffersFiles = files.Count>0 || DraggedFiles.Offered(data)`。
4. 沒有檔案：記 log（甩掉的格式清單）、`e.DragEffects = None`、return（donut 保持不顯示）。
5. 有檔案：`_dragOverEvents=0; _lastDragAt=null`；觸發 `FilesEntered(files)`；再呼叫一次 `OnDragOver`。
6. `FilesEntered` 處理（`RC:242-249`）：若 click mode、或檔案與目前相同（`SameFiles`：數量相同且路徑逐一以不分大小寫比對相等；或 `SameWebFiles`：全是 placeholder 且檔名序列相同）→ 忽略（同一批再進來不重播）。否則 `viewModel.Load(files, clickMode:false)` → `donut.PlayOpenAnimation()` → `WatchFlipKey()`（現在 `FlipsFromHook` 變 true）。

**C. 拖曳經過（DragOver）**（`RadialWindow.axaml.cs:75-87`）

- `at = e.GetPosition(Donut)`（邏輯像素，donut 控制項座標）；`hit = Donut.HitTest(at)`；計數；`Donut.SetExternalHit(hit, at)` 點亮 slice／觸發群組展開（§4.8）。
- **效果：** `e.DragEffects = (_dragOffersFiles && Donut.Converts(hit)) ? Copy : None`；`e.Handled=true`。
  - `Converts(hit)`：命中 `Segment` 且該 slice「非群組且已啟用」；或命中 `SubSegment` 且該子 slice 已啟用（`DonutMenu.Groups.cs:290-296`）。
  - **因此「drop 永遠 Copy」要更精確地說：從不回報 Move（避免 Explorer 在放下後刪除來源檔）；在可轉換的 slice 上回 Copy，其餘（洞、外圈、停用 slice、群組本身）回 None（顯示禁止游標）。** 依 OLE 慣例回 `DROPEFFECT_NONE` 時不會呼叫 `Drop`，而是 `DragLeave`；所以實務上「放在洞或停用 slice 上」＝取消，走 §2.2.E 的離開流程，而不是 `Dropped` 的 `Center` 分支（該分支存在但幾乎走不到）。
  - 預期的 Rust IDropTarget：`DragOver` 回傳 `DROPEFFECT_COPY`／`DROPEFFECT_NONE` 同上。（`DROPEFFECT_LINK/MOVE` 一律不要。）

**D. 放下（Drop）**（`RadialWindow.axaml.cs:96-109`、`RC:250-263`）

1. `hit = Donut.HitTest(e.GetPosition(Donut))`；記 `DragSummary`（log）；`Donut.CollapseGroup()`；`Donut.SetExternalHighlight(-1)`。
2. `e.DragEffects = Converts(hit) ? Copy : None`。
3. `dropped = DraggedFiles.Read(data, forDrop:true)`（這時才真正讀瀏覽器內容；`Web` 物件的所有權交給 `Dropped` 處理器）。
4. `Dropped(hit, dropped)`：`_dropped=true; WatchFlipKey()`（停止 hook 的 flip 監聽）；若 `hit.Kind ∈ {Segment, SubSegment}` → `Convert(hit.Index, hit.Sub, dropped.Files.Count>0 ? dropped.Files : viewModel.Files, dropped.Web)`；否則 `dropped.Web?.Dispose(); Close()`。
5. `Convert`（`RC:319-368`）：
   - `preset = viewModel.PresetAt(index, sub)`（停用或群組為 null）；`sources = SourcesFor(preset, files)`（資料夾只在「壓成單一封存檔」preset 才保留，否則濾掉，`RC:452-453`）；若 preset 為 null／sources 空／有「孤兒 placeholder」（`web==null` 但路徑是 placeholder）→ 釋放 web、`Close()`、結束。
   - 工具型 preset（`IsTool`）→ `Close(animate:false)` 後開對應工具視窗（web 檔案先下載再開）。
   - 合併型（merge PDF／影片串接／圖片合併，且 sources>1）→ `Close(false)`，開排序視窗（`ChooseMergeOrderAsync`）。
   - `Output.AskEveryTime`（且非 web）→ `Close(false)` 後跳另存對話框。
   - 其他：`conversions.Start(sources, preset, showInToast:false, web)` → `ShowProgress(job)`（donut 轉成進度環，見 §2.3）；`job==null` 則 `Close()`。
   - 轉換引擎細節不屬本規格。

**E. 離開與取消**

- `DragLeave`（`RadialWindow.axaml.cs:89-94`）：記 log；`Donut.ExternalDragLeft(_lastDragAt)`：若最後位置落在「透明縫隙」內（`LeftThroughSeam`，§4.8）則**忽略**（Windows 在拖放跨過透明像素或視窗形狀改變時會誤報離開），否則清高亮並收合群組。
- 按鍵放開 → `TriggerService.DragEnded` → `RC:141-163` `OnDragGestureEnded`：
  - 不是可見的拖曳模式 → 直接 return。
  - 若從未有拖放進入 donut：（節流 1/分鐘）記 log，內容區分「Filee 以系統管理員執行 → 被 UIPI 擋」(`Program.Elevation.ShellCannotReachFilee`，`Core/Platform/ElevationState.cs:35-36`)。
  - 若沒有檔案且不在進度模式 → `Close(animate:false)`（隱形視窗會擋點擊，馬上關）。
  - 否則等 250 ms（給 drop 一點時間到），若仍可見、未 drop、非 click mode → `Close(animate:true)`。
- 取消方式（拖曳模式）：把檔案拖到「洞」或視窗外再放開；放在停用的 slice；沒有任何拖放進來；放開按鍵 250 ms 後還沒 drop。Esc／點外面在拖曳模式不適用（視窗沒有鍵盤焦點）。
- `Close(animate)`（`RC:165-198`）：若在進度模式：有進度工作→`HandOffToToast()` 後 return；正在收尾動畫→return；否則 `LeaveProgressMode()`。其餘：`_visible=false; WatchFlipKey(); _grace.Cancel(); ReplaceWeb(null); CollapseGroup()`；animate 則 `PlayCloseAnimation(→Hide)`，否則直接 `Hide()`；最後 `OfferFormatRequest()`（若關閉時檔案都是 Filee 不認識的副檔名，每個副檔名每次執行期間提示一次「要求支援此格式」）。

### 2.3 進度環移交（與 donut 同視窗；細節供參考）

- 轉換開始後 donut 變成進度環（`DonutMenu.Progress.cs`），視窗保持在原位。常數：`RingRadius=36`、`RingStroke=6`、`CollapseSeconds=0.28`、`CollapseStagger=0.02`、`MinRingSeconds=0.6`、`HoldSuccessSeconds=0.9`、`HoldFailureSeconds=1.4`、`FadeSeconds=0.25`、`HandOffSeconds=0.22`；彈簧：環 pop-in `(300,16)`、勾／驚嘆號 badge `(420,14)`（`DonutMenu.Progress.cs:33-42, 195, 222`）。
- `HandOffAfter = 5 秒`：工作 5 秒仍在跑就交給 toast（`RC:49, 467-471`）；失敗立即交給 toast；Esc／點別處／新手勢也交給 toast。
- 下載徽章（瀏覽器檔案）：`RadialWindow.axaml:56-64`，位於 donut 上方 `translate(0,-70px)`，延遲 300 ms 顯示（細節在 ARCHITECTURE.md「Files dragged from a web browser」）。此段屬轉換／toast 的規格範圍，這裡不展開。

### 2.4 拖曳資料讀取（`DraggedFiles.Read`，`App/Services/DraggedFiles.cs`；行號以該檔計）

`Read(data, forDrop)`（:35-42）：先 `BrowserDrags.Read`（有檔→`DraggedItems(Placeholders, web)`）；否則 `Local(data)`：
- 取 `data.TryGetFiles()` 的本機路徑（Windows 即 CF_HDROP 等；Avalonia 內部，上游碼沒有細節 → 見 §7 風險），再退而求其次讀 macOS 的 `NSFilenamesPboardType`（Windows 不適用）。
- 處理（`Local`，:48-62）：`ResolveShortcuts`（.lnk 換成目標，目標須存在；`Win/ShellLinks.cs:28-41`，兩者同批則去重）→ `TrimEndingDirectorySeparator` → 必須存在（檔或資料夾）且**不是磁碟根目錄**（`C:\`）→ 不分大小寫去重。資料夾被允許進入（只有「壓縮成單一封存檔」會用到；其他 preset 在 `SourcesFor` 濾掉）。
- `Offered(data)`：有 `DataFormat.File`、或有 macOS filenames 格式、或 `BrowserDrags.MayOffer`（含 `FileGroupDescriptorW` 或任何「位址格式」）。

#### 瀏覽器拖曳規則表（`Core/Web/BrowserDrag.cs`、`App/Services/BrowserDrags.cs`）

資料來源格式（Windows 相關者；常數在 `Core/Web/BrowserDragFormats.cs:33-46`）：

| 格式名 | 內容 |
|---|---|
| `FileGroupDescriptorW` + `FileContents` | 虛擬檔案（Chrome／Edge 的圖片本體、任何連結的 `.url`）；descriptor 結構：4 byte 數量 + 每筆 592 byte，檔名在 offset 72 起 260 個 UTF-16 字元；offset 0 為 flags，offset 36 為屬性（`0x10`=資料夾）、offset 64/68 為大小 high/low（`BrowserDragFormats.cs:48-87`） |
| `UniformResourceLocatorW`／`UniformResourceLocator` | 連結或圖片位址 |
| `text/x-moz-url` | 「位址\n標題」成對（UTF-16） |
| `DownloadURL` | `mime:檔名:位址`（Chrome，頁面設定時，例：Gmail 附件） |
| `application/x-moz-file-promise-url` (+`-dest-filename`) | Firefox 圖片承諾 |
| `HTML Format` / `text/html` | 拖曳片段 |
| Firefox（Windows）另外會寫一份暫存檔到 `%TEMP%`（CF_HDROP） | |

規則（依序，第一個命中即回傳；`BrowserDrag.Files`，`BrowserDrag.cs:51-88`；文字開頭註解 :1-14）：

1. **瀏覽器自己交出位元組的檔案**（虛擬檔或 Firefox 暫存檔）：直接用，但**擋掉黑名單副檔名**（`WebFileNames.IsBlocked`）。黑名單（`Core/Web/WebFileNames.cs:19-24`）：`exe msi msix msixbundle appx appxbundle msp com scr pif cpl dll sys bat cmd ps1 psm1 psd1 vbs vbe js jse mjs wsf wsh hta reg inf lnk url scf application appref-ms gadget msc sh bash zsh csh ksh command tool run bin appimage desktop app dmg pkg mpkg kext workflow scpt applescript jar jnlp apk xapk ipa deb rpm snap flatpak py pyw pl rb`。（所以連結帶來的 `.url` 虛擬檔會被濾掉。）本機檔案若不是「Firefox 暫存檔」（`hasAddress` 且路徑在 `%TEMP%` 底下但不在 Filee 自己的工作資料夾）則整批視為一般本機檔，不當瀏覽器拖曳（`BrowserDrags.cs:71-83`、`IsTemporary` :105-118）。
2. **圖片**：HTML 片段恰好是一個 `<img>` 且無其他可見文字（`ImageOnly`，正則 `<img\b[^>]*?\bsrc\s*=\s*("…"|'…'|\S+)`，片段取 `<!--StartFragment-->`..`<!--EndFragment-->`，`BrowserDragFormats.cs:193-218`）→ 取 `src` 當位址（相對 src 以拖曳中的第一個連結位址為基底）。
3. Firefox 的 `file-promise-url` → 該位址（檔名取 `-dest-filename`）。
4. `DownloadURL` → 位址，檔名／mime 依它。
5. **連結**：依序嘗試 `text/x-moz-url` 的各位址、`UniformResourceLocatorW`、`UniformResourceLocator`、`public.url`、`_NETSCAPE_URL`、`text/uri-list`；去重；**只有位址結尾副檔名是 Filee 能轉的格式且不是網頁（`.html .htm .php .asp .aspx .jsp …`、或無副檔名）才收**（若同時是圖片拖曳則以「圖片」寬鬆規則收）。
6. **最後手段**：以上都沒有、但這個拖曳是圖片且有位址、拖曳資料帶像素（`blob:` 位址）→ 把像素存成 PNG（或 `public.jpeg/tiff/gif/webp`、`image/*` 位元組）。

`Address()` 的細節（`BrowserDrag.cs:115-151`）：`data:` 位址解碼，mime 須對到 Filee 認得的副檔名；否則位址必須是 http／https（絕對，或以頁面位址解相對）；檔名經 `Sanitize`（只留最後一段、去非法字元、非裝置名、≤120 字元、永不空）；黑名單→丟；網頁副檔名：若是圖片就去掉副檔名繼續（之後補 `.jpg`），否則丟；`FormatRegistry.Detect(fileName)` 成功→收；圖片或 mime 是圖片→補副檔名（mime 對照，否則 `jpg`）；mime 對得到可轉格式→補該副檔名；否則丟。
**不用讀純文字**（選取的位址是文字不是檔案）。
DragEnter 階段（`forDrop=false`）只要名字：Windows 可能先列出 `FileGroupDescriptorW` 的名字、還沒給位元組；這些以「名字 only」的 placeholder 顯示（`BrowserDrags.cs:92-97`）。
placeholder：`%TEMP%\Filee\web\<drop>\drop\<n>\<檔名>`，讓 donut 能不連網就依副檔名選 profile；`FilesEntered` 用 `SameWebFiles` 判斷是否同一批（ARCHITECTURE.md:77-121）。
副檔名／Detect 取自 `FormatRegistry`（格式表不在本規格範圍）。

---

## 3. 點擊／按鍵模式（Click mode）

### 3.1 觸發與讀取選取（`App/App.axaml.cs:94-122`）

`SelectionGesture`（Hold 或 KeyChord）處理：

1. 若 `_selectionPending`（上一次還在讀）或 `Settings.Paused` → 直接 return。
2. `_selectionPending=true`；`files = platform.GetFileManagerSelection()`（**在 UI thread 同步執行，須為 STA**；macOS 才包 Task.Run）。
3. 讀完再檢查 `!Paused`；`files.Count>0` → `radial.ShowForFiles(e.X, e.Y, files)`。
   - `e.X,e.Y`：Hold＝**按下滑鼠鍵時的位置**；KeyChord＝**最後一次滑鼠移動的位置**（`GestureDetector.cs:174, 192`）。
4. `files.Count==0`：節流（1 分鐘）記一行 log「selection shortcut found no selected files in {前景視窗 class / process}」並註明是否因系統管理員權限（`DescribeForegroundWindow`，`Win/WindowsPlatformServices.cs:229-233`）。**使用者看不到任何提示，什麼都不發生。**
5. 例外（COM 失敗以外的）→ log、開設定視窗的 `triggers` 頁並跳訊息框顯示 `ex.Message`。
6. `finally` → `_selectionPending=false`。

### 3.2 怎麼讀檔案總管選取（Windows，`Win/WindowsPlatformServices.cs:116-227`）

- 前景視窗 `GetForegroundWindow()`（hwnd）；0 → 空。
- 前景的 root（`GA_ROOT`）類別是 `Progman`／`WorkerW` → 讀桌面：`DesktopSelection()`；否則 `SelectionOf(foreground)`。
- **`SelectionOf`**（:165-227）：
  1. `Type.GetTypeFromProgID("Shell.Application")` → `shell.Windows()` 列出所有 Explorer／IE 視窗（COM 集合，每個 entry 一個 IWebBrowser2；Win11 分頁：**每個分頁一個 entry，HWND 都是頂層視窗的**）。
  2. 取 `activeTab = ExplorerTabs.ActiveTabOf(foreground)`：在前景視窗底下用 `FindWindowEx(topLevel, prev, "ShellTabWindowClass", null)` 列舉所有分頁視窗；回傳**第一個可見者**，沒有可見者回傳 z-order 第一個（`Win/ExplorerTabs.cs:25-35, 57-67`）。
  3. 每個 entry：取 `window.HWND`；若等於前景且 `activeTab != 0`，用 `IServiceProvider.QueryService(SID_STopLevelBrowser, IID_IShellBrowser).GetWindow()` 取該 entry 的分頁視窗 hwnd（`ExplorerTabs.TabOf`，:71-97）。IE 等取不到的 entry 吞掉例外。
  4. `ExplorerTabs.Candidates(entries, foreground, activeTab)`（:43-55）：只考慮 `TopLevel==foreground` 的 entries；若有 entry 的 Tab 等於 activeTab，**只用那一個**（別分頁的選取，即使過期，也不是使用者要的）；沒有分頁資訊就依序試同視窗所有 entry。
  5. 對候選 `window.Document.SelectedItems()` → 逐項取 `item.Path`，**只保留 `File.Exists(path)`**（`ExistingFiles`，:152-162）。第一個非空者回傳。
  - **選取到的資料夾會被濾掉**（只收檔案）；與拖曳模式（資料夾允許進來）不同。
- **`DesktopSelection`**（:125-149；`Win/DesktopShellView.cs:17-44`）：`ShellWindows`（CLSID `9BA05972-F6A8-11CF-A442-00A0C90A8F39`）的 `FindWindowSW(CSIDL_DESKTOP=0, 空, SWC_DESKTOP=8, out hwnd, SWFO_NEEDDISPATCH=1)` → `IServiceProvider.QueryService(SID_STopLevelBrowser, IShellBrowser)` → `QueryActiveShellView()` → `IShellView.GetItemObject(SVGIO_BACKGROUND=0, IID_IDispatch)` → `SelectedItems()` → 同樣只收 `File.Exists`。失敗（沒有 Explorer 桌面／Explorer 重啟中）回空。桌面圖示層（§1.4）**不**支援選取快捷鍵：因為它會去讀 Explorer 隱藏的桌面檢視（`:88-94` 註解），所以滑鼠手勢可以、選取讀取不行。
- 回傳後 `ShowForFiles` 會再做 `platform.ResolveShortcuts(files)`（`RC:116-117`，`web==null` 時）。

### 3.3 Donut 顯示（`ShowForFiles`，`RC:114-138`）

1. `files` 解捷徑後為空 → 結束。
2. `ReleaseProgressToToast(); ReplaceWeb(web)`（click mode 的瀏覽器 placeholder 屬 donut 所有）。
3. `forAnotherProcess && macOS` 才啟用 `ActivationGrace`（僅 macOS；**Windows 完全用不到**，見 `ActivationGrace.cs:1-34`：開啟後 1 秒內允許失去焦點一次而不關閉）。Windows 路徑：`_grace.Cancel()`。
4. `_dropped=false; _converted=false`；`viewModel.Load(files, clickMode:true)`；`FitToThisDonut()`；`AllowContextEdit=true`（右鍵 slice 可編輯 preset）。
5. `PlaceAndShow(window, x, y, activate:true)`（位置規則同 §2.1；`activate` 時呼叫 `window.Activate()`）；`WatchFlipKey()`；`PlayOpenAnimation()`。
6. 位置：Hold＝按下點；KeyChord＝最後游標位置；右鍵選單／Send To 的位置＝`TriggerService.LastCursor`，若 (0,0) 就用主視窗位置 +200（`App.axaml.cs:364-370`）；首頁拖放區＝放下點。

### 3.4 Click mode 的操作與關閉

- **滑鼠**：左鍵「按下→放開」在同一個 slice 才觸發（`DM:485-560`）：
  - 啟用的非群組 slice → `SegmentInvoked` → `Convert(index, -1, …)`；
  - 群組 slice（有子項）→ 立即展開（不等延遲）；
  - 群組環的子 slice（啟用）→ `SubSegmentInvoked`；
  - 洞 → `CenterInvoked` → `Close()`；
  - 外圈（Outside）且有群組展開 → 收合群組。
  - 右鍵：slice → 編輯該 preset（`SegmentContextRequested`；`AllowContextEdit` 時）；子 slice 同理。編輯期間 `_editing=true`，失焦不關閉；編輯完用 `viewModel.Load(…同樣檔案, 同一面)` 重載並重新 `Activate()`（`RC:532-551`）。
  - 滾輪：`FlipWithWheel` 時翻面（§4.7）。
- **鍵盤**（視窗有焦點才收得到；`RadialWindow.axaml.cs:111-124`、`DonutMenu.Groups.cs:209-284`）：
  1. 若 `FlipKey`（預設 `Tab`）與按鍵的 hook 名相等（不分大小寫、**含 Shift 等任何修飾鍵**）→ 翻面（`Flip()`），**一律吃掉這個鍵**（即使沒得翻）；`FlipKeyFromHook` 為 true 時（拖曳模式）視窗自己不翻，避免重複（#504）。
  2. 方向鍵：`Right/Down = +1`，`Left/Up = −1`。
  3. 若有群組環展開：`Esc` 或 `Backspace` → 先收合群組並把 hover 放回該群組 slice（Esc 不關 donut）；若同時有子項 hover（`_subHover>=0`）：`Enter/Space` → 該子 slice 啟用才觸發；方向鍵 → 在環內以循環方式移動子 hover（`(sub + step + count) % count`）。
  4. 無群組展開：`Enter/Space`（需已有 `ActiveIndex>=0`）：群組有子項→展開、`SetSub(0)`、`_ringFromKeys=true`；已啟用的 slice → `SegmentInvoked`；停用的什麼也不做（但按鍵被吃掉）。方向鍵：收合群組；若尚無 hover 則 `+1` 從 slice 0、`−1` 從最後一個開始，否則 `(current+step+Count)%Count` 循環；`PointAtSelection()`。
  5. 成功處理的鍵會設 `_keyboardHold=true; _keyAnchor=_lastPointer`：滑鼠要移動超過 `KeyboardJitter=6` px 才會把控制權拿回來（`DM:446-453`）。
  6. 其他：`Esc`（donut 層）→ `EscapePressed` → `Close()`。
  - **沒有數字鍵直選**（搜尋過 `DonutMenu*.cs`、`RadialWindow.axaml.cs` 的 `Key.D0-D9/NumPad`：未找到）。
- **失焦關閉**（`RC:270-285`）：`Deactivated` 且 click mode 且非編輯中 → `_grace.Forgives()`（Windows 恆為 false）→ `Close()`。點視窗外面也是因此關閉（視窗失去啟用）。
- **焦點寬限（`ActivationGrace`）**：僅 macOS 用（`forAnotherProcess && IsMacOS`，`RC:125-128`），Windows 無此行為。Windows 上的對應問題是：第二個 Filee 行程（右鍵選單啟動）要先 `AllowSetForegroundWindow(ASFW_ANY)` 把前景權限交給常駐行程，否則 donut 拿不到焦點、Esc 與點外面都關不掉（`Win/Foreground.cs:1-15`）。（屬右鍵選單範圍，列在風險 §7。）
- 關閉動畫：`PlayCloseAnimation`（§4.6）；`Motion.Enabled==false` 直接 `Hide()`。

---

## 4. Donut 繪製

### 4.1 設定與預設值

| 值 | 預設／範圍 | 出處 |
|---|---|---|
| `Donut.OuterRadius` | 150；套用時夾 [100, 240]（邏輯像素） | `Core/Settings/AppSettings.cs:63`、`RadialViewModel.cs:303` |
| `Donut.HoleRatio` | 0.42（設定頁 0.2–0.7）；控制項再夾 [0.2, 0.75] | `AppSettings.cs:66`、`DM:210` |
| `InnerRadius` | `OuterRadius * clamp(HoleRatio, 0.2, 0.75)` | `DM:210` |
| `Donut.Opacity`（slice 背景不透明度） | 0.96；夾 [0.1, 1] | `AppSettings.cs:69-73`、`RadialViewModel.cs:305` |
| 圓角 `SliceCornerRadius` | `clamp(SoftRadius.TopLeft, 0, 28) * 0.5`；`SoftRadius` = 主題 `CornerRadius`（預設 16）夾 [0,28] ⇒ 預設 **8** | `DM:1185`、`App/Services/SoftPalette.cs:107-110`、`AppSettings.cs:49` |
| `GapPixels`（slice 間隙，定寬） | 3 | `DM:93` |
| `PopOut`（hover 彈出） | 7 | `DM:94` |
| `OpenDuration` / `OpenStagger` | 0.42 s / 0.028 s | `DM:95-96` |
| `DragStartDistance`（編輯模式拖曳門檻） | 5 | `DM:97` |
| `MaxSlices`（單環最多） | 12 | `Core/Profiles/ToolbarProfile.cs:13` |
| `MaxSubPresets`（群組展開最多） | **6**，且子項不可為群組 | `Core/Presets/Preset.cs:126`、`RadialViewModel.cs:234` |

Slice 角度：從 12 點鐘順時針，slice i 的**中心角** `mid = i * span`，`span = 2π / count`（count≤0 時 2π）；也就是第一個 slice 置中在正上方（`DG:48-51`）。角度＝順時針、自上方起算的弧度；`PointAt(center, r, a) = (cx + r·sin a, cy − r·cos a)`；`AngleOf(c, p) = atan2(px−cx, cy−py)`，負則 +2π（`DG:54-62`）。

### 4.2 命中測試（`DG:116-124, 65-69`；`DM:364-388`）

`HitTest(center, p, inner, outer, count, slack=PopOut=7)`：
- `d = |p − center|`；`d < inner` → `Center`；`d > outer + slack` → `Outside`；`count == 0` → `EmptyRing`；否則 `Segment(IndexAt(angle))`。
- `IndexAt(angle, count) = floor((angle + span/2) / span) % count`（中心對齊，所以第 0 個 slice 涵蓋 [−span/2, +span/2)）。
- **命中測試用「銳角扇形」（不含間隙與圓角）**：間隙與被圓角切掉的角仍屬於該角度的 slice（`DG:129` 上方註解、`DG:128-138` 文件）。
- 內半徑門檻用 `inner`（= r1），不是畫出來的中心圓 `r1−6`。
- 控制項層（`DM:364`）：進度模式有自己的測試；編輯模式且 hover 的 slice 之 ✎ 按鈕半徑 13 優先；群組展開時先測子環（`SubIndexAt`），其次，若主測試得 `Outside` 但在 `inner(子環內半徑)` 以內且位於該群組的「扇形方向」（`InFan`）→ 視為該群組 slice（避免往環移動時越過間隙而收合）。

### 4.3 Slice 輪廓（`DG:139-215`, `DM:1187-1230`）

`Wedge(center, r1, r2, mid, span, gap=3, cornerRadius)`：
- 外緣半角 `outerHalf = max(0.001, span/2 − gap/2/r2)`；內緣 `innerHalf = max(0.001, span/2 − gap/2/r1)`。⇒ 兩側各留 1.5 px 定寬間隙（內緣角度較大）。
- 外起點 `PointAt(r2, mid − outerHalf)`；路徑：外弧順時針 → 直線到內緣終點 → 內弧逆時針回起點；弧的 large-arc 旗標＝`半角*2 > π`。
- 圓角：`radius = min(cornerRadius, (r2−r1)/2)`；`> 0.25` 才圓角。`Rounded()` 以幾何求出四角（外弧側兩角、內弧側兩角，與徑向邊相切），失敗（太細或太短）時用**二分搜尋 14 次**找最大可行的半徑，結果 `low > 0.25` 才採用，否則退回銳角（`DG:216-251`）。圓角只切入角落，slice 仍在銳角輪廓內，間隙不變。四角的求解數學在 `Corner()`（`DG:210-248`，圓與直線求交）；Rust 實作可改用任何能畫「等寬環扇形、圓角半徑 ≤ 計算上限」的方法（Direct2D 路徑或 tiny-skia），重點是**視覺一致**：預設圓角 8、定寬間隙 3。
- `span ≥ 2π − 0.001`（單一 slice）→ 畫整圈（EvenOdd 填充兩個圓：外半徑與內半徑）。
- `count == 0`（空 donut）→ 畫整圈並可被拖入（編輯模式用）。

### 4.4 Slice 繪製與狀態（`DM:839-923`）

每個 slice（`open ≤ 0.001` 或 `span < 0.01` 則略過）：
- 變換（由右往左套用）：`T(−center) → R(rotate) → S(scale·(1+hover·0.025)) → T(center + offset)`；
  - `scale = 0.55 + 0.45·open`；`rotate = (1 − min(open,1)) · −0.35` rad；
  - `offset = PointAt((0,0), hover·7, mid)`（沿徑向彈出，最大 7 px）。
  - 整體不透明度＝`clamp(open,0,1) × (被拖曳的 slice ? 0.3 : 1)`（0.3 僅編輯模式）。
- 填色：`SliceBrush` 的 alpha × `SliceOpacity` × （停用 ? **0.55** : 1）；描邊 1 px `SliceBorderBrush`（可為 null）。
- 強調：`emphasis = (i==dropTarget ? 1 : hover)`；`emphasis > 0.01` 且 slice 啟用 → 在上方再蓋 `AccentBrush`，alpha `0.82 × clamp(emphasis,0,1)`。**停用 slice 永不上強調色。**
- `hot = (i == ActiveIndex || i == dropTarget) && enabled`；標籤顏色：`hot ? OnAccentBrush : 啟用 ? Foreground : SecondaryForeground`（停用用次要文字色）。
- `ActiveIndex = externalHighlight >= 0 ? externalHighlight : hoverIndex`（拖放高亮優先於指標 hover）（`DM:182`）。
- 陰影（`DrawSoftShadow`，`DM:925-942`）：半徑 `r2+14` 的放射漸層橢圓，中心下移 4 px；漸層停點：`0 → α0`、`(r2−18)/R → α0`、`(r2−2)/R → α46/255 黑`、`1 → α0`；整體不透明度＝`clamp(mean(open),0,1) × ShadowCoverage`（翻面時為各 slice 目前寬度占比之和，`Flip.cs:160-174`）。

標籤（`DrawLabel`，`DM:1038-1058`）：
- 位置＝`(r1+r2)/2` 處、slice 中心角；**文字不旋轉，水平置中**；`maxWidth = span·(r1+r2)/2 − 10`，夾 [30,120]；`maxHeight = r2 − r1 − 16`。
- 名稱：SemiBold，字級 **13**，若有單字比行寬寬則縮小至最小 **10**（`LabelSize`：`13·(行寬−1)/最寬單字寬`，CJK 漢字／假名的詞不算；`DM:1068-1093`）；最多 2 行，字元省略。
- 副標（`Caption`，如目標格式）：字級 10.5 Regular，顏色 `hot ? OnAccent : Secondary`；僅在 `Caption` 非空、不等於 Label（不分大小寫）、且 `label.Height + 14 < maxHeight` 時顯示；行數 2 若 `label.Height + 28 < maxHeight` 否則 1。名稱與副標垂直一起置中於錨點。
- 韓文單詞內插 U+2060 避免在音節間斷行（`KeepKoreanWords`，`DM:1103-1117`）；右到左語言：控制項 `FlowDirection=LeftToRight`（slice 順序不變），文字依 UI 文化 RTL（`DM:141-143, 1155-1156`）。
- 字型：UI 字型鏈（Noto Sans＋各語系＋Segoe UI…，`SoftPalette.FontFamilies`）；Rust 版若不內嵌 Noto，可用 Segoe UI / Microsoft JhengHei UI 等系統字型。

### 4.5 中央樞紐（Hub）（`DrawCenter`，`DM:944-993`）

- 圓半徑 `(r1 − 6) · (1 + centerPulse·0.06)`；填 `CenterBrush`、描 `BorderPen`；`_externalCenterHot`（拖放懸停在洞）時再蓋 `DangerBrush` alpha 0.18。
- 文字（水平置中，最大寬 `radius·1.6`）：標題 14 SemiBold 最多 2 行（`CenterTitle`）；副標 11.5 Regular 最多 2 行（`CenterSubtitle`；`centerHot` 時用 `DangerBrush`）；下方提示 10.5 Regular，寬度上限 `maxWidth·0.85 − 圖示寬`，不透明度 0.85，與上文間隔 5（副標與標題間隔 2）。總高置中於圓心。
- 提示（`FlipHint`）：`"{鍵} back"`／`"{鍵} front"`（i18n）前面畫**自繪**的翻面箭頭（半徑 4.5 的圓弧，起點 −60° 掃 300° 順時針，端點兩筆箭頭，線寬 1.2，圓端）；只有滾輪能翻面（沒有鍵）時箭頭前再畫一隻小滑鼠（7×10，圓角，中間一小段滾輪線）。尺寸常數：`FlipArrowSize=9, MouseWidth=7, IconGap=4, MouseGap=6`（`DM:997-1036`）。不用字型字元「↻」是因為某些字型缺字會出現方塊（#554）。
- 中央文字內容（`RadialViewModel`）：
  - 拖曳模式等待檔案：`donut.center_drag`。載入後 `CenterTitle`：一種格式→`donut.files_same`（`{數量} × {格式顯示名}`），多種→`donut.files_mixed`（`{數量} files`）（`:179-181`）。數量來自「Shapefile 附屬檔視為同一個檔」處理後的清單（`CompanionFiles.WithoutCompanions`）。
  - 副標：click mode 預設 `donut.close_hint`（"Esc to close"）；拖曳模式為空。
  - 當 `ActiveIndexChanged`（hover／拖放懸停變化）時 `ShowHint(index, sub, centerHot)`（`:276-293`）：洞 hot→`donut.cancel`；群組且已啟用或有子項 hover：子項啟用→`donut.click_to`／`donut.drop_to`（`"{群組名} · {子項名}"`），子項停用→該子項 `DisabledReason`，無子項 hover→`donut.pick_in_group`；一般 slice：啟用→`click_to`／`drop_to`，停用→它的 `DisabledReason`；否則 click mode `close_hint`，拖曳模式 null。

### 4.6 動畫與彈簧（全部參數）

- **彈簧積分器**（`DM:577-606`）：半隱式歐拉；`force = k·(target−x) − c·v`；`v += force·h; x += v·h`；一幀被切成 `ceil(dt/(1/120))` 個子步（`MaxStep = 1/120` s）；結算條件 `|target−x| < 0.001 && |v| < 0.01` → 直接設為目標、速度 0。`dt` 每幀夾在 [0, 0.05] s，第一幀 `1/60`（`DM:709`）。
- 預設彈簧（k=320, c=20）；臨界阻尼 `CriticalDamping = 36`（`DM:574`）。
- 各彈簧：

| 動畫 | 參數 | 出處 |
|---|---|---|
| slice hover 彈出（目標 1，是當前 ActiveIndex） | k=320, c=20（欠阻尼，有一點彈） | `DM:751` |
| slice hover 放開（目標 0） | k=320, c=36（臨界，不反彈） | `DM:751` |
| 中央 pulse（`PulseCenter` 設初速 6，目標 0） | k=260, c=12 | `DM:252, 752` |
| 群組環展開（目標 1） | k=320, c=24 | `DM:755` |
| 群組環收合（目標 0） | k=320, c=36 | `DM:755` |
| 編輯模式 slice 中心角／寬度、預覽 slice | k=420, c=36 | `DM:346-359` |
| 進度環 pop-in | k=300, c=16 | `Progress.cs:195` |
| 進度 badge | k=420, c=14 | `Progress.cs:222` |

- **開啟動畫**（`PlayOpenAnimation`，`DM:215-229`；`Advance`，`DM:762-782`）：`_open[i]` 全設 0、時鐘歸 0；每幀 `t_i = clamp((clock − i·0.028) / 0.42, 0, 1)`，`_open[i] = EaseOutBack(t_i)`，`EaseOutBack(t) = 1 + 2.55(t−1)³ + 1.55(t−1)²`（c1=1.55, c3=2.55）。全部 `t≥1` 結束。
- **關閉動畫**（`PlayCloseAnimation`）：`t_i = clamp((clock − i·0.01) / 0.16, 0, 1)`，`_open[i] = 1 − t³`；完成後呼叫 callback（隱藏視窗）。`Count==0` 或 `Motion.Enabled==false` 直接 callback。翻面進行中先 `FinishFlip()`。
- **Reduce animations**（`App/Services/Motion.cs`）：`Motion.Enabled`＝`!(settings.Theme.ReduceMotion ?? OS 偏好)`；OS 偏好 Windows＝`SystemParametersInfo(SPI_GETCLIENTAREAANIMATION=0x1042)` 回 false 即「偏好減少動態」（`Win/WindowsPlatformServices.cs:247-248`）。跟隨系統時，**每次顯示 donut 前 `Motion.Refresh()` 重讀**（避免遠端工作階段／喚醒時一次誤報就永久關掉，`Motion.cs:21-38`, `RC:594`）。關閉動畫時：hover 直接等於目標、`_open` 直接 1（或 closing 時 0）、翻面直接換面、展開直接到位（`DM:728-745`）。
- **影格迴圈保險**（`DM:609-702`）：若 250 ms（`StalledLoopMs`）沒有新影格就視為停擺，重啟迴圈，最多重試 2 次（`FrameRetries`），仍不行就把所有動畫直接跳到結束狀態，確保 donut 一定顯示出來。Rust 若用 `WM_TIMER`／`DwmFlush`／VSync 驅動，仍要有類似保險。

### 4.7 正反面翻轉（`DonutMenu.Flip.cs`；`RadialViewModel.cs`）

- 每個 `ToolbarProfile` 有 `PresetIds`（正面）與 `BackPresetIds`（背面）；兩面各自走相同的 `PresetAvailability` 檢查（§5.2）。背面在正面畫出之後才用 `Dispatcher` Background 優先權檢查（`RadialViewModel.cs:166-176`），避免延遲首次顯示。
- 能不能翻（`CanFlip`）：另一面**至少有一個啟用的 slice**（`Face.IsUsable`，`:84`）；背面為空、全停用 → 不翻（Tab 沒有作用）。提示 `FlipHint`：`CanFlip && (有翻面鍵 || FlipWithWheel)` 才有，格式 `donut.flip_to_back`／`donut.flip_to_front`，`{0}` 為鍵的顯示名（`KeyNames.Display`，沒鍵則空字串 Trim）；沒有鍵但滾輪可翻→`FlipHintWheel=true`（顯示小滑鼠）（`:201-218`）。
- 翻面鍵：`DonutSettings.FlipKey`（預設 `"Tab"`；空字串＝無）。實際生效鍵 `EffectiveFlipKey(triggers)`（`AppSettings.cs:109-117`）：先清掉前後空白；若是保留鍵（`Escape, Enter, Space, Left, Right, Up, Down, Backspace`，`:91-95`）或與任一 KeyChord 手勢的鍵相同（因為拖曳時修飾鍵還按著，那個快捷鍵會被一起觸發）→ 退回 `Tab`；`Tab` 也不行就是無鍵。
- 滾輪：`FlipWithWheel`（預設 true）。**同一次轉動（相鄰事件間隔 < `WheelQuietMs = 300` ms）只翻一次**（`Flip.cs:46, 97-102`）。Click mode：滾輪事件由 donut 控制項收（`Delta.Y != 0`，吃掉事件，`:105-112`）。
- **拖曳模式沒有鍵盤焦點**，所以 `TriggerService.WatchFlipKey` 開啟時由 hook 回報：`FlipKeyPressed`（keydown，**過濾自動重複**：記 `_flipKeyDown`，只在 up→down 時觸發，`TriggerService.cs:94-107`）與 `FlipWheelTurned`（垂直滾輪、`Rotation != 0`，**每個 notch 都回報**，由 donut 的 `FlipFromWheel(TickCount64)` 去抖，`:110-114`）。hook **不攔截**按鍵或滾輪（Explorer 在拖曳中本來就忽略 Tab；滾輪會同時被 Explorer／下層視窗收到）。`RC:557-566`：`WatchFlipKey = _visible && !_dropped && !IsClickMode && HasFiles`。
- **翻轉動畫**：`SliceTurn = 0.12 s`，`Stagger = 0.08 s`，總長 `FlipDuration = 0.20 s`（`Flip.cs:50-52`）。
  - `Flip()`（:275-313）：進度模式／關閉中／編輯模式 → 不翻；翻轉進行中再按＝切換「結束後再翻一次」旗標（按兩次抵銷）；`!CanFlip` → false；`!Motion.Enabled` → 收合群組、直接換面。
  - 否則：`CaptureOldFace()`（存舊面 slice 的 mid/span/open/hover/emphasis/hot/dimmed，以及展開中的群組環）→ `BeginWave` → 收合群組 → **立即**換成新面（`FaceSwapping` 事件 → `RadialViewModel.ToggleFace()`；命中測試與放下永遠屬於已換上的新面）→ `PrepareIncoming` → `_flipClock=0` → 動畫。
  - 波的起點角度 `FlipOrigin`：外部拖曳最後位置的角度 > 指標位置的角度（非鍵盤佔用時）> 目前 `ActiveIndex` 的中心角 > 0（`BeginWave`，:184-196）。
  - 每個 slice 的延遲 `Delay = (index==起點slice ? 0 : Stagger · |IEEERemainder(mid − origin, 2π)| / π)`（:176-182）；新面 slice 的延遲額外取「與其角度重疊的舊面 slice 之延遲」的最大值（讓新的接在舊的之後翻出，`PrepareIncoming`，:198-210）。
  - 進度 `u = clamp((clock − delay)/0.12, 0, 1)`；舊面只畫 `u < 0.5`、新面只畫 `u ≥ 0.5`；寬度因子 `max(|cos(πu)|, 0.01)`；變換＝繞「該 slice 的徑向軸」壓縮寬度：`T(−c) · R(−mid) · S(width, 1) · R(mid) · T(c)`（:134-158）。即每個 slice 像繞自己的輻條翻牌（樞紐不動）。
  - 翻轉中指標移動會「指向」某個角度：把該角度的舊 slice 與新 slice 的延遲拉近到目前時鐘，讓使用者指著的那一格優先翻到（`PointAtAngle`，:233-262）。
  - 結束時 `EndFlip()`；有排隊的翻轉則再 `Flip()` 一次。
  - 換面後（`SwapFaces`，:378-405）：外部拖曳／指標位置重新命中並點亮；鍵盤選取歸零（方向鍵從第一格開始）；觸發 `ActiveIndexChanged`，樞紐文字跟著新面更新。
- 編輯器的 Front／Back 切換用 `PlayFlipIn`（只播進入波，`_flipClock = SliceTurn/2`，:319-330），不在本規格範圍。

### 4.8 群組（group）slice 與子環（`DonutMenu.Groups.cs`）

- 群組 slice：`DonutItem.Children != null`（`IsGroup`）；`CanExpand` ＝有子項。群組 slice 的 Label＝群組名、Caption＝`donut.group_caption`（"Group of {n}"）、`IsEnabled = 任一子項啟用`、`DisabledReason`：無子項→`donut.group_empty`，否則取第一個子項的停用原因（`RadialViewModel.cs:256-270`）。
- 展開的子環（最多 6 個非群組子項）：
  - 半徑 `inner = R + PopOut(7) + SubGap(6) = R+13`；`outer = inner + SubBand(46) = R+59`（`Groups.cs:66`）。
  - 每個子 slice 的角寬 `SubSpan(parentSpan, count, midRadius)`：`parentSpan·(1 + 2·0.25)/count`（涵蓋父 slice 與兩側鄰居各 25%）；若 `midRadius > 0` 則至少 `MinSubWidth(44 px)/midRadius`；上限 `2π/count`（`DG:74-91, 85`）。子 slice 中心角 `parentMid + (k − (count−1)/2)·subSpan`（`DG:96`）。
  - 命中：`SubIndexAt(center, p, innerR, outerR, parentMid, count, subSpan)`：距離需在 [inner, outer]；`delta = IEEERemainder(AngleOf(p) − parentMid, 2π)`；`sub = floor(delta/subSpan + count/2)`，須在 [0,count)（`DG:103-113`）。
  - 繪製（`DrawGroupRing`，:410-437）：以「父 slice 與內半徑交會的點」為錨做縮放 `0.8 + 0.2·t`，不透明度 `clamp(t,0,1) × 整體開啟度`，`t` = 展開彈簧值；每個子 slice 用同樣的 wedge／間隙 3／圓角／填色（停用 ×0.55）；子環文字：中心半徑 `(inner+outer)/2`，`maxWidth = subSpan·mid − 8`，`maxHeight = SubBand − 8 = 38`，**只畫名稱（`caption:false`，名稱用 `RingName`，即 NameKey+`.short`）**；hover 時蓋 `AccentBrush` α0.82、文字用 OnAccent。
- 展開時機（`TrackGroups`，:104-165）：
  - 指標／拖放停在群組 slice 上 **160 ms**（`ExpandDelay`）後展開；或在該 slice 上「往外」移動 ≥ **10 px**（`OutwardPixels`，從最靠近圓心的距離起算）立即展開；或從該 slice 往外一撥到子環位置（`Outside` 且在 `InFan` 且距離 ≤ `SubRadii.Outer + 8`）立即展開。
  - 在洞、其他 slice（非子環扇區上的鄰居外半部）、或繞到外面離開扇形方向 → 收合；在去子環途中經過的鄰居 slice 外半部（`距離 > (r1+r2)/2` 且在 `InFan`）→ 子環保持展開（但此時點擊／放下仍算那個鄰居 slice）。
  - 點擊群組 slice（click mode）／Enter／Space 立即展開，不等延遲。
- 透明縫隙補洞：群組展開或即將展開時，額外畫 **alpha=1/255 的幾乎透明填充**（`HitBackdrop`，`Color.FromArgb(1,0,0,0)`）：一個半徑 `R+7` 的圓、加上一個由 `R` 到 `reach = SubRadii.Outer + 8` 的扇形（寬度 `min(2π, subSpan·count + 2·8/reach)`，無圓角）（`:341-360`）。原因同 §2.1：Windows 讓滑鼠與拖放穿過全透明像素。
- `LeftThroughSeam(p)`（:366-389）：指標或拖放「離開」事件的最後位置若在 donut 內離邊緣 ≥ `LeaveEdge = 12` px（`distance ≤ R+7−12`），或在補洞扇形內且離兩側邊 ≥ 12 px，視為穿過透明縫隙而不是真的離開（不收合群組、不清高亮）。
- `Esc`／`Backspace` 先收子環（見 §3.4）。

### 4.9 顏色來源（theme 資源）

控制項預設值被 `App/Styles/Controls.axaml:12-23` 以 `DynamicResource` 綁定：

| DonutMenu 屬性 | 資源 | 淺色預設 | 深色預設 | 出處 |
|---|---|---|---|---|
| `SliceBrush`、`CenterBrush` | `SurfaceBrush` | `#FFFFFF` | `#262624` | `Styles/Soft.axaml:16, 42` |
| `SliceBorderBrush` | `SubtleBorderBrush` | `#16000000` | `#1CFFFFFF` | `Soft.axaml:18, 44` |
| `Foreground` | `TextPrimaryBrush` | `#1F1E1D` | `#F1EFE8` | `:19, 45` |
| `SecondaryForeground` | `TextSecondaryBrush` | `#6B6A66` | `#AEACA4` | `:20, 46` |
| `DangerBrush` | `DangerBrush` | `#C73A39` | `#F09595` | `:21, 47` |
| `SuccessBrush` | `SuccessBrush` | `#0F7A5A` | `#5DCAA5` | `:22, 48` |
| `AccentBrush` | `AccentBrush` | 設定的強調色，預設 `#7F77DD` | 同 | `App/Services/SoftPalette.cs:15, 66-69` |
| `OnAccentBrush` | `OnAccentBrush` | 強調色亮度 > 0.6 → `#1F1E1D`，否則白 | 同 | `SoftPalette.cs:73`（亮度函式在同檔） |
| `CornerRadius` | `SoftRadius` | 主題圓角（預設 16） | | `SoftPalette.cs:107-110` |

- 使用者可用 `%APPDATA%\Filee\theme.json` 覆寫 `surface/border/text/textSecondary/danger/success`（見 `docs/CUSTOM-THEMES.md:1-79`）；單色強調（monochrome accent）時淺色 `#1F1E1D`／深色 `#F1EFE8`（`SoftPalette.cs:87-96`）；Material You 風格有另一套（`App/Services/MaterialYouPalette.cs:35-50`）。主題模式 System／Light／Dark（`ThemeSettings.Mode`）。移植第一版只需 Soft 風格的上表即可。
- 其他固定色：陰影黑 α46/255；強調疊色 α0.82；停用 ×0.55；危險疊色（洞懸停）α0.18；HitBackdrop α1/255。

---

## 5. Profile 選擇與 PresetAvailability

### 5.1 Profile 選擇（`ProfileSelector.Select`，`Core/Profiles/ToolbarProfile.cs:74-105`）

輸入：`counted` = `CompanionFiles.WithoutCompanions(files)`（Shapefile 的 `.shx .dbf .prj .cpg .sbn .sbx .fbn .fbx .ain .aih .atx .ixs .mxs .qix .qmd .qpj .shp.xml` 若與同名 `.shp` 同批，視為該 `.shp` 的一部分；單獨存在的附屬檔照一般檔案算。`Core/Formats/CompanionFiles.cs:14-75`；web 位址不算本機）。

1. `extensions` = `counted` 每個檔案的 `FormatRegistry.ExtensionOf(path)`（去點；已知雙段副檔名如 `tar.gz` 視為一個；其餘取最後一段），不分大小寫去重。
2. 若 `extensions` 非空：
   - 先找**第一個** `!IsFallback` 且 `extensions` 每個都被它的 `Extensions` 包含（`Matches`，不分大小寫、容許前導點）的 profile；
   - 沒有 → 第一個 `IsFallback && Extensions.Count>0` 且全含的「混合選取 profile」。
3. 仍沒有（或 `extensions` 空）→ `CatchAll`：第一個 `IsFallback && Extensions.Count==0` → 否則第一個 `IsFallback` → 否則 `profiles.FirstOrDefault()`。
4. `profile == null`（沒有任何 profile）→ PresetIds 當空清單（donut 無 slice）。

profile 順序＝設定（`profiles.json` 之類）清單順序；內建 profile 資料在 `Core/Presets/BuiltInData.cs`（`CreateProfiles`），不在本檔範圍；Rust 版需把內建 profile、內建 preset 資料搬過去（見交叉規格）。
格式偵測：`ProfileSelector.DetectFormats(counted)` = `FormatRegistry.Detect(path)`（以副檔名查表，查不到的丟掉）後 `Distinct`，供中央標題與 availability 使用（`ToolbarProfile.cs:103-104`）。

### 5.2 一個 slice 的建立（`RadialViewModel.BuildFace`，`:225-273`）

- `presets = presetIds.Select(store.FindPreset)` 並丟掉找不到的。
- 一般 preset → `DonutItem{ Id, Label = DonutName(p), Caption, IsEnabled = (reason==null), DisabledReason = reason }`：
  - `Label`＝`NameKey+".donut"` 的 i18n（沒有就退回 `DisplayName`）；自訂名稱（`Name` 非空）直接用 Name（`Core/Localization/ILocalizer.cs:46-53`）。
  - `Caption`：目標格式為「同來源」（`BuiltInData.SameAsSource = "*"`）→ 前 2 種來源格式的顯示名以 `/` 連接；否則 `FormatLabels.NameOf(loc, p.TargetFormat)`。與 Label 相同則不顯示（見 §4.4）。
- 群組 → §4.8（子項 `Take(6)`、排除群組）。
- 停用原因 `reason = availability.Check(preset, formats)`；`null` 表示可用。

### 5.3 `PresetAvailability.Evaluate` 完整規則（`App/Services/PresetAvailability.cs:105-247`）

輸入：preset、`formats`（`FileFormat` 清單，每個有 `Id`、`Category`）。輸出：`(Use, Reason)`；`Check` 只回傳 `Reason`（null＝可用）。`Use` 為 `Usable`／`NeedsPackage`／`Unusable`；NeedsPackage 與 Unusable 都會讓 slice 停用，差別只在原因字串。依序檢查：

1. **`IsClipTool`**（剪輯影片）：`videos = formats ∩ Category==Video`。空→`Unusable "error.unsupported_source"`。有影片能被剪（`ClipTool.CanCut(fmt, planner)` = 影片格式 && `planner.Plan(source, TargetFor(source))` 恰好 1 步，`Core/Presets/ClipTool.cs:24-26`）→ 可用。否則找「尚未安裝的引擎包」使其可行→`NeedsPackage "engine.reason.needs_package"`（`{0}`＝包名）；沒有→`Unusable "error.no_route"`。
2. **`IsImageEditTool`**：任一格式 `Category==Image`（`ImageEditTool.CanEdit`）→ 可用；否則 `error.images_only`。
3. **`IsHideTool`**：任一格式 `HideService.CanOpen(id)`（= `HideTool.CanHide(id) && ImageEncoder.Sources.Contains(id)`，`HideService.cs:21-22`）→ 可用；否則 `error.images_only`。
4. **其他工具 preset（`IsTool`）**：
   - `Id == "tool-sheet-studio"`：`formats` 非空且全是 `xlsx` → 需要 LibreOffice 轉換器 `GetStatus().IsAvailable`，是→可用；否則有未安裝的含 `libreoffice` 的包→`NeedsPackage`，沒有→`error.no_route`。非 xlsx→`error.unsupported_source`。
   - `Id` 以 `tool-pdf-` 開頭：`formats` 非空且全是 `pdf` 才可用，否則 `error.unsupported_source`。
   - `IsCropTool`（`Tool` 為 null 或 CropTool）且 `formats` 非空且全部屬於**可寫的單幀光柵** `{jpg, png, webp, bmp, avif, jxl, jp2, tga, qoi, dds, pcx}`（`:31-34`）→ 可用。
   - 其餘 → `error.unsupported_source`。
5. **`PacksIntoOneArchive`**（壓成單一封存檔，任何檔案皆可含未知格式）：`FormatRegistry.Get(TargetFormat).Id` 對應的封存格式有任一 `IFileCombiner.CanCombineInto` → 可用，否則 `error.no_route`。
6. `formats.Count == 0` → `error.unsupported_source`。
7. **目標＝同來源 且 `Image.EditsPicture`**（縮放、灰階、裁透明、去背、極致壓縮、改速度、自訂尺寸、最小短邊）：`formats` 限縮為 `Category==Image`；空→`error.images_only`。
8. **`Image.TrimTransparent`** 且所有 `formats` 的輸出格式都不能存 alpha（`ImageEncoder.HoldsAlpha(目標或來源id)`）→ `error.no_transparency`。
9. **`JoinsImages`**（合併圖片）：需要 `IImageJoiner`，且（目標非同來源時）`joiner.CanWrite(目標)`，否則 `error.no_route`；`pictures = formats ∩ {Image, Vector}` 空→`error.images_only`；任一圖片 `joiner.CanRead(id) || planner.CanConvert(id,"png")` → 可用；否則 NeedsPackage／`error.no_route`。
10. **`JoinsIntoOneMedia`**（合併影片／音訊／圖片轉影片）：`Joinable`：投影片模式→圖片類且（為 png 或能轉 png）且 `png→目標`可行；非投影片→（影片類，或音訊類且目標為 `m4a`）且 `planner.CanConvert(來源, 目標)`。可行→可用；否則 NeedsPackage／`error.no_route`。
11. **一般轉換 `Possible(planner)`**：`formats` 中**任何一個**可行即可用（一個 preset 對混合選取只需涵蓋其中一部分）。每個來源格式 f，`target = (同來源 ? f.Id : TargetFormat)`：
    - 合併 PDF 且 f 是 pdf 且不旋轉、不 OCR → 恆可行（原樣併入）。
    - `frames = SavesFramesOf(f.Id)`、`motion = ExtractsMotionOf`、`live = MakesLivePhotoOf`、`ocr = RecognizesTextOf`（見 `Preset.cs:157-190`）。
    - 不旋轉／翻轉：`planner.CanConvert(f.Id, target, frames, motion, live, ocr)`。
    - 旋轉／翻轉（`Image.TurnsOrMirrors`）：必須 `planner.Plan(…)` 的路徑含**會轉圖的步驟**（`FirstTransformStep() >= 0`）才算可行；若有路徑但沒有轉圖步驟，記 `copiesUnturned=true`。
    - 以「已安裝引擎」的 planner 判斷；可行→可用。
    - `copiesUnturned` 且不可行 → `error.cannot_turn`。
    - 否則尋找「尚未安裝且裝了之後可行」的引擎包→`engine.reason.needs_package`（`{0}`＝包名）；沒有→`error.no_route`。

**原因字串 i18n 鍵與英文／繁中**（`App/Assets/i18n/en.json`／`zh-TW.json`）：

| key | en | zh-TW |
|---|---|---|
| `error.unsupported_source` | This file type isn't supported | 不支援此檔案類型 |
| `error.no_route` | No engine can do this conversion | 沒有引擎可以完成此轉換 |
| `error.cannot_turn` | Rotate and flip work on images only | 只能旋轉和翻轉圖片 |
| `error.images_only` | Works on images only | 只能用於圖片 |
| `error.no_transparency` | This format has no transparency, so there's nothing to trim | 此格式沒有透明部分，無需裁切 |
| `engine.reason.needs_package` | Needs {0}. Install it in Settings → Engines | 需要 {0}，請在「設定 → 轉換引擎」中安裝 |
| `donut.group_empty` | No presets left in this group. Edit it in Settings → Donut toolbar | 此群組中已沒有預設，請在「設定 → 環形工具列」中編輯 |
| `donut.center_drag` | Bring files here | 把檔案拖到這裡 |
| `donut.files_same` | {0} × {1} | {0} 個 {1} |
| `donut.files_mixed` | {0} files | {0} 個檔案 |
| `donut.drop_to` | Drop → {0} | 放開 → {0} |
| `donut.click_to` | Click → {0} | 點擊 → {0} |
| `donut.pick_in_group` | Move outward to choose | 向外移動以選擇 |
| `donut.cancel` | Drop here to cancel | 拖到這裡取消 |
| `donut.close_hint` | Esc to close | 按 Esc 關閉 |
| `donut.group_caption` | Group of {0} | 群組 · {0} 個項目 |
| `donut.flip_to_back` | {0} back | {0} 背面 |
| `donut.flip_to_front` | {0} front | {0} 正面 |
| `donut.same_format` | Same format | 同格式 |

（其餘 16 種語言檔在同目錄 `*.json`；編輯模式用的 `donut.edit`、`donut.empty` 不在本規格範圍。）

### 5.4 Availability 依賴的資料，以及能否用 `filee-cli formats <fmt>` 替代

availability 需要的資料來源：
1. **FormatRegistry**：`FileFormat{ Id, DisplayName, Extensions[], Category, Writable }`、`Detect(path)`、`FindById`、`FindByExtension`、`ExtensionOf`（含雙段副檔名表 `CompoundExtensions`）。
2. **RoutePlanner**（`Core/Conversion/RoutePlanner.cs`）：Dijkstra 在「轉換器邊」上找最低成本路徑（≤3 步）；`CanConvert(from, to, frames, motionPhoto, livePhoto, ocr)`、`Plan(...)`（回傳含各步 `Converter`）、`FirstTransformStep()`。依賴「引擎已安裝與否」（`ConverterCatalog.CreatePlanner()` 只含已安裝引擎；`CreatePlanner(ids)` 指定引擎集合）。
3. **引擎包狀態** `EnginePackageState{ IsInstalled, Package.ConverterIds, Name }`（用來在 `NeedsPackage` 裡指名要安裝哪個包）。
4. **Preset 欄位**：`TargetFormat`、`Kind(Group/Tool/…)`、`Tool`、`Image.*`、`Pdf.*`、`Media.*`、`Archive.CombineIntoOne` 等（`Core/Presets/Preset.cs`）。

CLI 的 `filee formats <fmt>`（`Filee.Cli/Cli.cs:325-357`）輸出格式：

```
{DisplayName} ({ext1, ext2, …}) converts to:
  {id1}, {id2}, …            ← 已安裝引擎能做的目標（或 "  (nothing with the engines installed now)"）
With optional engines (install them in Filee, Settings → Engines):
  {idA}, {idB}, …            ← 只有「再裝引擎包才能做」的目標（可能沒有這段）
```

- `<fmt>` 接受格式 id 或副檔名（去前導點、轉小寫，先查 id 再查副檔名，`Cli.cs:289-293`）。
- 清單為 `FormatRegistry.Known` 中 `Writable` 且不是來源自己、不是 `Folder` 的格式 id；以 `CreatePlanner().CanConvert(from, to)`（**所有旗標預設 false**）判斷。
- **能替代的部分**：一般轉換（§5.3 第 11 步的「不旋轉」分支，且 frames／motion／live／ocr 皆 false 的情況）：`target ∈ now` ⇒ Usable；`target ∈ later` ⇒ NeedsPackage（但**只知道需要「某個」引擎包，CLI 沒給包名**，所以 `{0}` 要另想辦法：可固定顯示成通用文字，或另外取引擎包資料）；兩者都不在 ⇒ `error.no_route`。「同來源」目標就是 `Usable` 只要來源格式可寫（`target == from`，`planner.CanConvert(from, from)` 對 CLI 不在清單內，需另外決定：上游 `planner.CanConvert(id,id)` 是否為真未在 CLI 輸出中，**需實測或讀 RoutePlanner**）。
- **不能替代的**：(a) 旋轉／翻轉需要「路徑含轉圖步驟」；(b) 需要旗標的轉換（存每一幀、動態照片擷取／製作 Live Photo、OCR）；(c) 工具型 preset 的專屬規則（clip 需「剛好 1 步」、crop 的光柵清單、`tool-pdf-*`、sheet-studio 要 LibreOffice）；(d) 合併型（圖片 joiner、FFmpeg joiner、PDF 合併、封存 combiner）；(e) `EditsPicture` 與 `TrimTransparent` 的圖片限制（這些純粹靠格式 `Category` 與 `HoldsAlpha` 表，可用內建靜態表重現）；(f) 引擎包名稱。
- 建議策略（供第一版）：把 `filee formats <每個可寫來源格式>` 的輸出在啟動時（或每次引擎變動時，而非每次顯示 donut）解析成 `HashMap<source_id, {now:Set, later:Set}>` 快取，並從 `filee formats`（無參數）取得 id／displayName／副檔名／Category／可寫表（輸出行格式：`  {id,-10} {DisplayName}{" (read only)"}  .ext1 .ext2`，依 Category 分段，`Cli.cs:349-355`）；(a)–(e) 的特例規則用 Rust 內建靜態表實作。註：CLI 每次叫用都要啟動 .NET，所以務必快取。

---

## 6. 與 §1–§5 對應的上游檔案清單（已讀）

見回報檔案清單。未找到／未讀者：

- `libuiohook` Windows 後端（原生 C）：未在 `upstream/` 內（只有 src/、docs/）。修飾鍵 mask 的建立方式、`MouseMoved` 事件帶的 mask 是否包含按住的滑鼠鍵，**只能依 Filee 的使用方式推定**（`TriggerService.cs:323, 372-378`）。
- Avalonia 的 Windows OLE `IDropTarget` 實作與 `TryGetFiles()` 具體讀哪些剪貼簿格式：不在上游碼，屬 Avalonia 內部（見 §7）。
- `BuiltInData.CreateProfiles`（內建 profile 內容）、`FormatRegistry` 完整格式表、引擎包清單：非本份範圍（交叉規格）。
- 進度環繪製細節（`DonutMenu.Progress.cs` 其餘部分）只摘常數，未逐行讀。

---

## 7. 移植風險與難點（Rust／Windows）

1. **在別的程式 drag 進行中接收 OLE DragEnter**。必須自己 `OleInitialize`（STA）+ 對 donut 視窗 `RegisterDragDrop(hwnd, IDropTarget)`；`DragOver` 要極快（每次滑鼠移動都呼叫，需要同步做命中測試並回 `DROPEFFECT_COPY/NONE`）。`IDataObject` 的取用：`CF_HDROP`（本機檔案）、`Shell IDList Array`（`CFSTR_SHELLIDLIST`，Explorer 虛擬資料夾／搜尋結果會只有它）、`FileGroupDescriptorW` + `FileContents`（lindex 逐筆，`TYMED_ISTREAM`／`TYMED_HGLOBAL`）、`UniformResourceLocatorW`、`text/x-moz-url`、`DownloadURL`、`HTML Format` 等自訂格式（`RegisterClipboardFormat`）。DragEnter 階段只讀名字／路徑，不要搬位元組（虛擬檔讀取可能阻塞並影響來源 app，Filee 刻意延到 Drop，`BrowserDrags.cs:48-50`）。Avalonia 內部怎麼讀 `TryGetFiles()` 沒有在上游碼，需自行實測 Explorer／Total Commander／Directory Opus 各自提供哪些格式。
2. **透明置頂不啟用的視窗＋per-pixel alpha**。需要 `WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_LAYERED`（或 DirectComposition／`UpdateLayeredWindow`）、`ShowWindow(SW_SHOWNOACTIVATE)`、`SetWindowPos(HWND_TOPMOST, …|SWP_NOACTIVATE)`；click mode 則要真的 `SetForegroundWindow`（見 3）。上游沒有看到 `WS_EX_NOACTIVATE`／`TOOLWINDOW` 的 P/Invoke（`Win/NativeMethods.cs` 只有 `SetWindowPos` 常數），是 Avalonia 的 `ShowActivated=False`／`ShowInTaskbar=False` 替它做；Rust 要自己設，而且要確認 `NOACTIVATE` 視窗仍能收到 OLE drop（一般可以）。**注意：全透明像素（alpha=0）會讓滑鼠與拖放穿過。** 必須維持 §2.1／§4.8 的 `alpha=1/255` 底板與群組補洞，或用 `WS_EX_LAYERED` + `SetLayeredWindowAttributes` 以外的做法（DComp 與 `UpdateLayeredWindow` 的 hit-test 規則不同，要實測）。
3. **低階 hook**。`WH_MOUSE_LL`／`WH_KEYBOARD_LL`：callback 必須立即回傳（`LowLevelHooksTimeout`，Windows 10 1709+ 預設 300 ms，上限 1000 ms；超時 hook 被靜默移除，上游註解提到約 1 秒），因此只入列；放事件消化於另一執行緒；hook 所在執行緒需有 message loop；還要處理 hook 被系統拔掉後的重掛（上游以 `HookDisabled` 事件＋文件記載「系統拔掉 hook」的情境，`RC:69-73`）。不要吞事件（回傳 `CallNextHookEx`）。修飾鍵與按住的滑鼠鍵狀態要自己追蹤（libuiohook 內部做了，Rust 沒有）；注意 Alt 的 `WM_SYSKEYDOWN`、左右 Ctrl／Shift、Win 鍵（Meta）。滑鼠座標已是實體像素（需 PerMonitorV2 manifest）。
4. **Hook 看不到比自己高權限的視窗輸入（UIPI），且 Filee 以系統管理員執行時 Explorer 的 drop 到不了 Filee**。上游偵測 `ShellCannotReachFilee` 並寫 log（`ElevationState.cs:35-36`），不會嘗試繞過；Rust 版同樣建議不以管理員啟動，並在診斷中提示。
5. **Explorer 選取讀取需要 COM STA 與 late-binding**。上游用 `Shell.Application`/`IWebBrowser2`/`IShellBrowser` 並以 `dynamic` 呼叫 `Document.SelectedItems()`、`item.Path`。Rust 可用 `windows` crate 的 `IShellWindows`／`IWebBrowser2`／`IShellFolderViewDual`（`IDispatch` 動態呼叫較麻煩，可改走 `IShellBrowser::QueryActiveShellView` → `IFolderView2::GetSelection`/`Items(SVGIO_SELECTION, IShellItemArray)` 取 `SIGDN_FILESYSPATH`）；行為差異要小心：上游只回 `File.Exists` 的項目（資料夾被濾掉），且有 Win11 分頁（`ShellTabWindowClass`）處理；桌面走 `FindWindowSW(CSIDL_DESKTOP, SWC_DESKTOP)`。必須在 STA thread 做，且 COM 呼叫可能阻塞數百 ms（上游在 UI thread 同步做；Rust 建議丟 worker STA thread 並用 `_selectionPending` 同款防重入）。
6. **click mode 的焦點**。Windows 前景鎖：只有目前持有輸入的行程能 `SetForegroundWindow`。常駐行程是在 hook 事件之後被動叫出視窗，通常因為「剛收到使用者輸入（hook 事件）」可取得前景權；若失敗要用 `AttachThreadInput` 或送一個假 Alt 鍵（`keybd_event(VK_MENU)`）技巧。第二行程（右鍵選單）要先 `AllowSetForegroundWindow(ASFW_ANY)`（`Win/Foreground.cs`）。失焦即關閉的行為依賴 `WM_ACTIVATE(WA_INACTIVE)`/`WM_KILLFOCUS`。
7. **自繪的 2D 與動畫**。上游全用 Skia（Avalonia）手繪，未使用子控制項。Rust 可用 Direct2D（`ID2D1PathGeometry`＋變換）或 `tiny-skia`/`vello`+DComp。需要：路徑弧（SVG 風格 arc）、放射漸層、圓角扇形（§4.3 的幾何求解）、文字排版（多行、最大寬度、字元省略、置中、RTL、CJK 斷行；DirectWrite `IDWriteTextLayout` 對應）、整體 alpha 群組（`PushOpacity`）、每 slice 變換（旋轉＋縮放＋平移、翻面時「沿輻條壓縮」的非等比縮放）。影格驅動要自己做（`DwmFlush`／`IDXGISwapChain`／`WM_TIMER` 16 ms）並保留 §4.6 的停擺保險。
8. **DPI 與座標**。hook 座標＝實體像素；視窗大小與 donut 內部座標＝DIP；跨螢幕不同縮放（PerMonitorV2）時，視窗要依「游標所在螢幕」的縮放決定 `sidePx`，顯示後可能因 `WM_DPICHANGED` 重算，故上游顯示前後各設一次位置（`RC:594-598`）。clamp 用 `WorkingArea`（避開工作列）。
9. **拖曳中 Explorer 的行為前提**：視窗必須在 Explorer 的 `DoDragDrop` 迴圈中被 `WindowFromPoint` 命中；donut 在 Explorer 開始拖曳之後才顯示（晚到），因此必須是最上層且 `alpha≥1` 才命中（§2.1）。若 Filee 的 donut 被別的 topmost 視窗蓋住（或被 Windows 壓到下面），拖放到不了——`RaiseTopmost` 必須每次顯示都呼叫。
10. **OLE 回傳 `DROPEFFECT_NONE` 就不會有 Drop**（§2.2.C）。實作者勿期待 Drop 到洞上會進 `Dropped(Center)`；取消一律從 `DragLeave` + 按鍵放開（250 ms）進入關閉。另外 Windows 會在拖放跨過透明像素時送 `DragLeave`、又在進入時再送 `DragEnter`，需要像 `LeftThroughSeam` 一樣濾掉誤報，否則群組環展開前就收合（Filee 為此畫了 α=1 的補洞）。
11. **KeyChord 位置初值**：從未有滑鼠移動時 `LastCursor=(0,0)`，donut 會被夾到工作區左上角。Rust 實作建議在啟動時以 `GetCursorPos` 初始化（行為改善，與上游差異需記錄）。
12. **瀏覽器拖曳細節**（§2.4）：Chrome／Edge 的虛擬檔需要 `IDataObject::GetData(FileContents, lindex)` 並在 Drop 時一次讀完；Firefox 會寫暫存檔；`DownloadURL` 與 `blob:` 圖片像素；任何下載相關（HTTP、大小上限、名稱消毒）屬別份規格。
13. **執行緒模型**：上游依賴 Avalonia 的單一 UI thread（hook 事件 `Post` 到 UI thread）。Rust 版要決定 UI/OLE/COM 都在同一個 STA thread（建議），hook 事件消化執行緒用 channel 把「已判斷 scope」的手勢事件丟到 UI thread，且 scope 判斷中的 `GetProcessById`／`WindowFromPoint` 要在事件執行緒做（它們可能慢，不能放在 OS hook callback 內）。
14. **防毒相容**：全域 hook＋置頂透明視窗＋模擬輸入在部分防毒（含 Bitdefender 的行為偵測）可能被警告；請勿使用 `SendInput` 偽造輸入、不注入其他行程；hook 與自編未簽章 exe 的「常駐／開機自啟」在這台機器已知會被攔（見使用者全域指示的防毒表），建議第一版只在 `target\release\` 原地跑，不做自動啟動。

---

## 附：關鍵行為速查（給實作者的 checklist）

1. 預設手勢：Ctrl+左鍵拖曳 ≥ 系統門檻（逐軸，通常 4 px）→ 在游標處（以該事件位置為中心）顯示不可見、不搶焦點、可接收拖放的視窗。
2. 檔案進 DragEnter → donut 才「綻放」出現（開啟動畫 0.42 s、每片延遲 0.028 s）；沒有檔案（文字、框選）永遠不出現；按鍵放開且無檔案 → 立刻關；10 秒保險。
3. 只有「啟用的非群組 slice」與「啟用的子 slice」會回 `DROPEFFECT_COPY`；其他回 `NONE`；永不回 Move。
4. Tab（或自訂鍵）／滾輪翻面（總長 0.2 s；滾輪相鄰 300 ms 內算同一次）；拖曳中由全域 hook 監聽、不吞事件、鍵不重複。
5. Ctrl+Alt+Space → 讀前景 Explorer（含分頁、桌面）選取（只收檔案）→ click mode donut 在最後游標處；Esc／失焦關閉；方向鍵、Enter／Space、Backspace、無數字鍵。
6. Hold（預設未啟用，需設定）：按住滿 `HoldMilliseconds` 且移動 ≤ 門檻 → 讀選取並顯示 click mode donut 在按下位置。
7. scope：Drag/Hold 看游標下頂層視窗；KeyChord 看前景視窗；FileManager 範圍＝Explorer 類別／桌面類別／已知第三方檔案管理員程序名；Drag 另允許瀏覽器；排除程序與 Filee 自己永遠不觸發；`Paused` 全停。
8. 幾何：`R=150`、`hole=0.42`、間隙 3、圓角 8、pop-out 7、群組子環 `R+13 .. R+59`、子項 ≤ 6。
