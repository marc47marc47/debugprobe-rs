# SWD 訊號邏輯取樣流程（OLED 波形 + 偵測）

探針(layer 1, RP2040)如何在 host 閒置時自主取樣 SWCLK/SWDIO、畫成 OLED 波形,
並順帶偵測 layer 2 晶片。對應檔案:`src/logic.rs`、`src/main.rs`(`idle_scan`/`WaveRing`/
`TargetShared`/`oled_task`)、`src/display.rs`。

```
                          ┌─────────────────── 探針 RP2040 (PIO0) ───────────────────┐
  目標 SWD                │                                                          │
  ┌──────┐   GP2 SWCLK    │   ┌────────────┐  驅動 SWCLK/SWDIO   實體腳              │
  │layer │◄──────────────┼───│ SM0  (SWD)  │───────────────►(GP2 / GP3)            │
  │  2   │   GP3 SWDIO    │   │ swd_transfer│                    │  │                │
  │ MCU  │◄─────────────►┼───│ 主控 / 讀寫 │                    │  │  輸入同步器     │
  └──────┘               │   └────────────┘                    ▼  ▼  (兩腳永遠可讀) │
                          │   ┌────────────┐  in pins,2  ┌───────────────┐          │
                          │   │ SM1 (擷取)  │◄────────────│ GP2=bit0 SWCLK│          │
                          │   │ .wrap:      │  每取樣 2bit │ GP3=bit1 SWDIO│          │
                          │   │  in pins,2  │             └───────────────┘          │
                          │   │ autopush 32 │  (clk_sys/(DIVIDER=8) ≈ 15.6 MSa/s)    │
                          │   └─────┬──────┘                                         │
                          │         │ RX FIFO (每 32bit = 16 取樣) 滿即推            │
                          │         ▼                                                │
                          │   ┌────────────┐  dma_into(buf)                          │
                          │   │ DMA_CH0     │──────────────► buf:[u32; CAP_WORDS=16] │
                          │   └────────────┘                 (= SAMPLES 256 取樣)    │
                          └──────────────────────────────────────────────────────────┘
                                                    │
   ── core0: dap_task → idle_scan() 每輪編排 ───────┼───────────────────────────────
                                                    ▼
   adaptive_sweep() 找 used 速率(single-drop 1000→…→20k,失敗才 RP multidrop)
                                       │
                       每輪都擷取一窗(不論有無目標):
                                       ├─ cap.start(); xfer = cap.dma_into(&buf)
                                       ├─ dap.swd_wakeup(); dap.swd_read_dpidr()
                                       │     ← SM0 驅動 SWCLK/SWDIO 當「刺激」
                                       │       SM1 同時取樣到 buf
                                       ├─ select(xfer, Timer 20ms); cap.stop()
                                       ├─ count_edges(&buf) → (clk_e, dio_e)
                                       │     TARGET.set_edges(...)   ← OLED 訊號 log
                                       └─ WAVE.push(&buf)
                                                    │
                  ┌─── WaveRing (static WAVE) ──────┼──────────────┐
                  │  clk:[AtomicU32;4]  (128 欄×1bit)               │
                  │  dio:[AtomicU32;4]  (128 欄×1bit)               │
                  │  pos:AtomicU32      (最舊欄 = 讀取起點)          │
                  └─────────────────────────────────┬──────────────┘
                                                    │ load_clk/load_dio/pos
   ── core1: oled_task() 每 250ms ──────────────────┼───────────────────────────────
                                                    ▼
                  DebugOled::render(&OledModel{chip,flash,clk,dio,pos,scale})
                    line1 晶片型號 / 核心   line2 RDP   line5 速率/訊號儀
                    波形: 由 pos 起(最舊→最新)逐欄畫方波 → 畫面向左捲動
                                                    │
                                                    ▼
                                       SSD1306 128×64 OLED (I2C1 GP6/GP7)
```

## 關鍵點

- **SM1 只「讀」GP2/GP3 的輸入同步器**(`in pins,2`),不驅動腳;**驅動是 SM0 的事**。
  波形量到的是「**探針自己腳上**的電位」。
- 取樣是 SM1 在 **SM0 驅動 DPIDR 的同時**進行(`swd_wakeup`+`swd_read_dpidr` 當刺激),
  所以波形/邊緣數反映真實 SWD 交易。
- `DIVIDER=8` → 約 15.6 MSa/s;`CAP_WORDS=16` → 256 取樣;取乾淨 32 欄推進 ring。

## OLED 訊號 log:SWCLK/SWDIO 邊緣數（探針死活診斷）

無目標時,OLED 第 5 行顯示 `Ce{clk邊緣} De{dio邊緣} {頻率}k`:

| 顯示 | 判讀 |
|---|---|
| `Ce0 De0 …` | **探針沒驅出 SWCLK**(GP2 pad 死 / latch-up)→ 探針硬體壞 |
| `Ce>0 De0 …` | 探針有時脈,但 **SWDIO/目標無回應**(線路/目標/SWDIO) |
| `Ce>0 De>0` 但仍 no target | 有雙向跳變但 DPIDR 對不上(協定/parity/速率邊際) |
| `DP../16 AP../16 {f}k` | 已偵測到目標,顯示連線品質與鎖定速率 |

> SWCLK 由探針自驅,**與有無目標無關**——故 `Ce` 是判斷探針 GP2 輸出死活的決定性指標。
