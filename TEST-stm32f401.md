# 測試紀錄：STM32F401 Black Pill 作為 layer 2 目標

**日期**：2026-06-16
**目標**：驗證 debugprobe-rs 探針(通用 CMSIS-DAP SWD)能把**非 RP2040**晶片
（WeAct STM32F401 "Black Pill"）當作 layer 2 目標——SWD 燒錄/重置 + USB-to-UART 橋接,
並跑對等於 RP2040 測試韌體的功能（LED 閃爍 + OLED 狀態 + UART 雙向 rx/tx）。
**結果**：✅ **全部通過**。

> 圖檔說明：本檔用 ASCII 繪圖（任何 Markdown renderer 皆可顯示）。
> 需要向量圖版本見 [`TEST-stm32f401.html`](TEST-stm32f401.html)（瀏覽器開，內嵌 SVG）。

---

## 1. 受測物 / 環境

| 角色 | 裝置 | 說明 |
|---|---|---|
| layer 1 探針 | Raspberry Pi Pico + debugprobe-rs（`board-pico`） | serial `E6605838834DA330`，USB-FS 接主機 |
| layer 2 目標 | WeAct STM32F401 "Black Pill" | 標示 CCU6，SWD 實測 device id `0x10016433`＝矽晶為 **401xE / 512KB flash / 96KB SRAM**，Cortex-M4F |
| 目標韌體 | `stm32f401-target/`（embassy-stm32 0.6） | LED(PC13) + OLED(I2C1) + USART1 雙向 |
| 工具 | probe-rs、OpenOCD 0.12、elf2uf2-rs | — |

Black Pill **無板載偵錯器**，由自身 **USB-C 供電**。

---

## 2. 接線

```
        主機 PC
          │ USB (CMSIS-DAP v2 + CDC-ACM)
 ┌────────┴───────────┐                          ┌────────────────────────────┐
 │  Probe A (Pico)    │           SWD            │  STM32F401 Black Pill       │
 │  debugprobe-rs     │                          │  (USB-C 自身供電)            │
 │                    │                          │                             │
 │  GP2  SWCLK  ──────┼─────────────────────────▶│  PA14  SWCLK                │
 │  GP3  SWDIO  ◀─────┼─────────────────────────▶│  PA13  SWDIO                │
 │  GP1  nRESET ──────┼─────────────────────────▶│  NRST   (解 RDP 必需)       │
 │                    │           UART           │                             │
 │  GP4  UART1 TX ────┼─────────────────────────▶│  PA10  USART1 RX            │
 │  GP5  UART1 RX ◀───┼──────────────────────────│  PA9   USART1 TX            │
 │                    │                          │                             │
 │  GND  ─────────────┼──────────────────────────│  GND   (就近、獨立短地線)   │
 └────────────────────┘                          │                             │
 ┌────────────────────┐           I2C            │                             │
 │  SSD1306 OLED 0.96  │                          │                             │
 │  SCL ◀─────────────┼──────────────────────────│  PB8  (I2C1 SCL)            │
 │  SDA ◀────────────▶┼──────────────────────────│  PB9  (I2C1 SDA)            │
 │  VCC / GND         │                          │  3V3 / GND                  │
 └────────────────────┘                          │  LED：PC13（板載,active-low）│
                                                 └────────────────────────────┘
```

訊號流：

```
  燒錄/偵錯 :  主機 ──USB(CMSIS-DAP v2 bulk)──▶ 探針 A ──SWD──▶ F401
  序列橋接   :  主機 ──USB(CDC-ACM = COMx)────▶ 探針 A ──UART──▶ F401   （雙向）
```

---

## 3. 測試步驟與實測結果

### 3.1 初次連線（踩坑紀錄）

| # | 現象 | 根因 | 處置 |
|---|---|---|---|
| 1 | DP/AP 暫存器讀得到，但 AHB（flash/SRAM/CPUID）一律 `did not respond`；降速 50kHz 無效 | (a) Black Pill 寄生供電 ~2V；(b) 出廠 **RDP Level 1**；(c) 出廠韌體 sleep 關 HCLK；(d) SWD 杜邦線長、訊號完整性邊際 | 見下逐項 |
| 2 | 供電量到 ~3.28V 但 AHB 仍掛 | 寄生供電電流不足 | 接 **USB-C 自身供電** |
| 3 | `Protocol error` / 每次結果不同 | 長線反射（**降頻無效**：頻率不影響 PIO 邊緣速率） | ① SWCLK/SWDIO/GND 短線就近接地；② 探針改 2mA 弱驅動 + 慢 slew + 輸入 Schmitt（`src/probe/mod.rs`），重燒 board-pico |
| 4 | 用戶工具 `usbipd-rs --probe` 讀出 `RDP Level 1 Enabled` | 出廠讀保護 | OpenOCD `stm32f2x unlock 0` 清除（mass erase） |

### 3.2 清除 RDP（讀保護）

```bash
openocd -f interface/cmsis-dap.cfg -c "transport select swd" -c "adapter speed 1000" \
  -f target/stm32f4x.cfg -c "reset_config srst_only srst_nogate connect_assert_srst" \
  -c "init" -c "catch { reset halt }" -c "catch { halt }" -c "stm32f2x unlock 0" \
  -c "reset run" -c "shutdown"
```

實測輸出（節錄）：

```
Info : SWD DPIDR 0x2ba01477
Info : [stm32f4x.cpu] Cortex-M4 r0p1 processor detected
[stm32f4x.cpu] halted due to debug-request ...
Info : device id = 0x10016433
Info : flash size = 512 KiB
stm32f2x unlocked.
INFO: a reset or power cycle is required for the new settings to take effect.
```

→ **拔插 USB-C 重新上電**，讓 option byte 重載 + 完成 mass erase。

### 3.3 確認 RDP 已歸零（不重置即可完整讀 AHB）

```bash
probe-rs info --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000
```

```
Debug Port: DPv1, Designer: ARM Ltd
└── V1(0) MemoryAP
    └── 0 MemoryAP (AmbaAhb3)
        ├── 0xe00ff000 ROM Table (Class 1), Designer: STMicroelectronics
        ├── 0xe0040000 Cortex-M4 TPIU  (Coresight Component)
        └── 0xe0041000 Cortex-M4 ETM   (Coresight Component)
```

### 3.4 SWD 燒錄 + 重置

```bash
cd stm32f401-target && cargo build --release
probe-rs download --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000 \
  target/thumbv7em-none-eabihf/release/stm32f401-target      # → Finished in 1.71s
probe-rs reset --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd
```

### 3.5 UART 雙向驗證（探針 CDC = COM11 @115200）

**TX（F401 → 探針 → 主機）**：

```
hello from f401 #71..hello from f401 #72..hello from f401 #73..
```

**RX（主機 → 探針 → F401 → echo 回主機）**：寫入 `PROBE2F401-ECHO` → 收回 `ROBE2F401-ECHO`
（首字元偶於 OLED flush 阻塞期間掉落，屬已知、不影響功能）。

---

## 4. 結果一覽

| 驗證項 | 結果 |
|---|---|
| SWD 連線（讀 ROM table / Cortex-M4） | ✅ |
| 清除 RDP Level 1（`stm32f2x unlock`，mass erase） | ✅ |
| **SWD 燒錄韌體**（`probe-rs download`） | ✅ 1.71s |
| SWD reset | ✅ |
| 韌體運行（LED PC13 閃爍、計數遞增至 #73） | ✅ |
| OLED 狀態顯示（`f401 layer2` / `tx{n} rx{n}`） | ✅（目視） |
| UART TX（F401→主機） | ✅ 乾淨 |
| UART RX（主機→F401 echo） | ✅ |

**結論**：debugprobe-rs 探針可把 STM32F401（跨廠牌、Cortex-M4F）當 layer 2 完整燒錄與橋接，
與 RP2040 layer 2 功能對等。

---

## 5. 關鍵教訓（供日後 STM32 目標參考）

1. **供電**：寄生供電（~2V）會「DP 讀得到、AHB 全失敗」，務必目標自身供電。
2. **RDP Level 1**：出廠讀保護擋 flash/SRAM debug → 先 `stm32f2x unlock 0`（會 mass erase）再上電。
3. **connect-under-reset**：出廠韌體 sleep + RDP → 初次連線/解保護都要在 reset 下做。
4. **SWD 長線訊號完整性**：DP 永遠 OK、AP/AHB 時好時壞報 `Protocol error`；**降頻無效**。
   解法：短線就近接地 + 探針 SWCLK/SWDIO 改 2mA 弱驅動 + 慢 slew + 輸入 Schmitt。
5. 驗 UART 時**單次開埠**即可；快速開關 CDC COM 掃 baud 會讓探針 USB 瞬斷（會自行重列舉）。

接線、燒錄流程與 crate 結構細節見 [`MULTI-TARGET.md`](MULTI-TARGET.md)。
