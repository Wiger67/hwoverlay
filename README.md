# hwoverlay

輕量的遊戲用硬體監控浮動視窗（Windows）。永遠置頂、滑鼠可穿透、不搶焦點、不出現在工作列。

| 項目 | 來源 |
|---|---|
| CPU 使用率 | PDH `Processor Information\% Processor Time`（CPU 有多忙，與新版工作管理員「效能」分頁一致） |
| CPU 功耗 | PDH `Energy Meter(RAPL_Package0_PKG)\Power` |
| CPU 溫度 | PDH `Thermal Zone Information(*)\High Precision Temperature`（ACPI 熱區，取最高的那個） |
| GPU 使用率 / 功耗 / 溫度 / VRAM | NVIDIA NVML（驅動內建 `nvml.dll`）；非 NVIDIA 時改用 PDH `GPU Engine` 只顯示使用率 |
| RAM | `GlobalMemoryStatusEx` |
| 上傳 / 下載速度 | PDH `Network Interface(*)\Bytes Sent/Received/sec` |
| 整機功耗 (BAT) | PDH `Power Meter`，只有用電池時才有數值 |

## 編譯

```bash
cargo build --release
```

產出 `target/release/hwoverlay.exe`（約 400 KB）。

exe 的 icon 由 `build.rs` 在編譯時嵌入（`assets/hwoverlay.ico`，需要 Windows SDK 的 `rc.exe`，裝 Rust MSVC 工具鏈時通常已經有了）。找不到資源編譯器時一樣能編譯，只是沒有 icon。icon 的原始設計在 `assets/icon.svg`（大尺寸）和 `assets/icon-small.svg`（32px 以下用，線條加粗）。

## 使用

直接執行 `hwoverlay.exe`。

| 快捷鍵 | 功能 |
|---|---|
| Ctrl+Shift+F9 | 顯示 / 隱藏 |
| Ctrl+Shift+F10 | 換到下一個螢幕角落 |
| Ctrl+Shift+F11 | 移動模式：視窗出現黃框，可以用滑鼠拖到任何位置（含其他螢幕），靠近邊緣會自動吸附；**按右鍵開設定選單**；再按一次 F11 鎖定 |
| Ctrl+Shift+F12 | 結束程式 |

平常（非移動模式）視窗一樣是滑鼠穿透的，不會擋到遊戲的點擊。

### 設定（右鍵選單 / hwoverlay.ini）

在移動模式（Ctrl+Shift+F11）下對視窗按右鍵：

- 勾選要顯示的項目：CPU、GPU、VRAM、RAM、網路、功耗。**取消勾選的項目連資料都不會讀取**，例如關掉 GPU 就不會呼叫 NVML，所以關越多越省資源。
- **橫排顯示**：改成一行精簡版，例如
  `CPU  23%  55°C │ GPU  41%  63°C │ RAM  48% │ ↓ 1.2M ↑  85K │ PWR  77W`
  （橫排省略 GB 明細和各晶片瓦數，只留主要數字）
- 鎖定位置

所有設定（含位置）都存在 exe 旁邊的 `hwoverlay.ini`，第一次執行會自動產生，只有設定改變時才寫入。也可以用記事本直接改，改完按 Ctrl+Shift+F11 進入移動模式就會重新讀取：

```ini
[display]
horizontal=0
cpu=1
gpu=1
vram=1
ram=1
net=1
power=1
[window]
position=corner 0
```

`position` 是 `corner 0`～`corner 3`（左上、右上、右下、左下），或拖曳後的 `pos X Y`。刪掉 ini 就回到預設值。如果 exe 放在沒有寫入權限的資料夾（例如 Program Files），設定不會被記住。

`hwoverlay.exe --dump` 會在終端機印出 3 次數值，方便除錯。

字體大小、透明度、更新頻率、顏色還是在 `src/main.rs` 最上面的常數改。

## 注意

- 遊戲請用「無邊框視窗 / Borderless」模式；獨佔全螢幕下一般視窗疊不上去。
- `PWR` 是 CPU + GPU 晶片功耗，不是插座端的總耗電。插電時軟體無法得知變壓器實際輸出瓦數；拔掉電源用電池時 `BAT` 會顯示整台筆電的耗電。
- 要開機自動啟動：`Win+R` → `shell:startup`，把 exe 的捷徑放進去。
