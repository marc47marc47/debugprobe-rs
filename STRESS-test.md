# debugprobe-rs 極限效能 / 壓力測試報告

實機量測 debugprobe-rs（Rust/Embassy，多核心版）的**最大負荷**。
- 探針 A：Raspberry Pi Pico（RP2040 @125MHz），跑 debugprobe-rs，USB Full-Speed 接主機。
- 目標 B：Raspberry Pi Pico（RP2040），跑 `src/bin/uartecho.rs`（UART0 全速 echo）。
- 接線：A↔B 原廠 SWD（GP2/3）+ UART（A.GP4/5 ↔ B.GP1/0）+ GND，杜邦線。
- 工具：probe-rs、OpenOCD（`dump_image`/`load_image` 報傳輸率）、PowerShell SerialPort。

> 註：所有「最大值」是**此杜邦線接線下**的實測上限；更短/更佳走線可能更高。

## ST-1 — SWD 時脈與吞吐

SWCLK = clk_sys/(4×divider)，divider 為整數（`src/probe/mod.rs`）。請求 kHz 會對應到實際 SWCLK：

| 請求 | 實際 SWCLK (÷div) | 讀吞吐 (dump 128KB) |
|---|---|---|
| 1 MHz | 0.98 MHz (÷32) | 44 KiB/s |
| 8 MHz | 7.8 MHz (÷4) | 86 KiB/s |
| 12 MHz | 10.4 MHz (÷3) | 88 KiB/s |
| 16–24 MHz | **15.6 MHz (÷2)** | **94 KiB/s** |
| 31.25 MHz | 31.25 MHz (÷1) | ❌ 失敗 |

- **最高穩定 SWCLK ≈ 15.6 MHz**（÷2）。31.25 MHz（÷1）在杜邦線上失敗（訊號完整性）。
- **讀吞吐 ~94 KiB/s**（約 8–10MHz 後即 plateau）。
- **寫吞吐 ~203 KiB/s**（@15.6MHz，load_image 128KB）。
- **瓶頸**：USB Full-Speed + DAP 每筆開銷，**非 SWCLK**（故 >10MHz 吞吐不再上升）。寫 > 讀，因寫可 pipeline、不需逐字回讀。

## ST-2 — UART 橋接最高 baud 與吞吐

host 寫資料 → 探針 → 目標 echo → 回傳 host；分塊 round-trip 量測（含完整性比對）。

| baud | round-trip 吞吐 | 遺失/錯誤 |
|---|---|---|
| 921600 | 82.6 KB/s | 0 ✅ |
| 2 000 000 | 172 KB/s | 0 ✅ |
| **2 500 000** | **209 KB/s** | **0 ✅** |
| 3 000 000 | — | ❌ 掉資料 |

- **最高無遺失 baud ≈ 2.5 Mbaud**（3 Mbaud 開始掉位元組）。
- **峰值 round-trip 吞吐 ~209 KB/s @2.5M**（≈ 全雙工線速；單向有效 ~250 KB/s）。
- **瓶頸**：3M 時雙向合計 ~600 KB/s 超過橋接 256B UART 緩衝 + USB-FS CDC 可持續量 → overrun 掉位元組。

## ST-3 — 持續 soak 穩定度（多核心驗證）

連續 30 × 128KB SWD 讀（@15.6MHz）：

- **成功 30/30，錯誤 0**，總讀取 3.84 MB，耗時 38s。
- soak 後 `probe-rs info` 仍正常（AP 存取無退化）。
- 全程 OLED 在 **core1** 持續更新，與 core0 的密集 DAP 互不干擾 → **多核心重載穩定**。

## ST-4 — DAP 指令速率

| 模式 | 速率 |
|---|---|
| 單字傳輸（single `read_memory`，5000 筆）| **~3 576 transfers/s** |
| 區塊（TransferBlock，讀）| ~24 000 words/s（= 94 KiB/s）|
| 區塊（TransferBlock，寫）| ~52 000 words/s（= 203 KiB/s）|

- 單筆受 **USB Full-Speed 往返延遲**限制（~3.6k/s）；批次傳輸靠 TransferBlock 大幅攤平開銷。

## 結論：最大負荷一覽

| 面向 | 最大負荷 | 瓶頸 |
|---|---|---|
| SWD 時脈 | **15.6 MHz**（31.25MHz 失敗）| 杜邦線訊號完整性 |
| SWD 讀 | **~94 KiB/s** | USB-FS + DAP 開銷 |
| SWD 寫 | **~203 KiB/s** | USB-FS + DAP 開銷 |
| UART baud | **2.5 Mbaud**（無遺失）| 橋接緩衝 + USB-FS CDC |
| UART 吞吐 | **~209 KB/s** round-trip | 同上 |
| DAP 單筆 | **~3.6k transfers/s** | USB-FS 往返延遲 |
| soak | **30/30、0 錯誤、3.84MB** | — 穩定 |

整體效能由 **USB Full-Speed（12 Mbps）+ DAP/CDC 協定開銷**主導，與 C 版同級（同硬體/同 USB-FS）。
多核心版在極端 SWD + UART 負荷下穩定，OLED(core1) 不影響 core0 偵錯/橋接。
