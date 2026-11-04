# hwoverlay

輕量的遊戲用硬體監控浮動視窗（Windows）。永遠置頂、滑鼠可穿透、不搶焦點、不出現在工作列。

| 項目 | 來源 |
|---|---|
| CPU 使用率 | PDH `Processor Information\% Processor Utility`（與工作管理員一致） |
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

產出 `target/release/hwoverlay.exe`（約 370 KB）。

## 使用

直接執行 `hwoverlay.exe`。

| 快捷鍵 | 功能 |
|---|---|
| Ctrl+Shift+F9 | 顯示 / 隱藏 |
| Ctrl+Shift+F10 | 換到下一個螢幕角落 |
| Ctrl+Shift+F12 | 結束程式 |

`hwoverlay.exe --dump` 會在終端機印出 3 次數值，方便除錯。

字體大小、透明度、更新頻率、顏色都在 `src/main.rs` 最上面的常數改。

## 注意

- 遊戲請用「無邊框視窗 / Borderless」模式；獨佔全螢幕下一般視窗疊不上去。
- `PWR` 是 CPU + GPU 晶片功耗，不是插座端的總耗電。插電時軟體無法得知變壓器實際輸出瓦數；拔掉電源用電池時 `BAT` 會顯示整台筆電的耗電。
- 要開機自動啟動：`Win+R` → `shell:startup`，把 exe 的捷徑放進去。
