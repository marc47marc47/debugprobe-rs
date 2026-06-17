# debugprobe-rs

Raspberry Pi **Debug Probe** 韌體的 **Rust / Embassy** 重寫，從原始 C 韌體（FreeRTOS + TinyUSB +
Pico SDK，保留在 `debugprobe/` 子目錄）逐步移植而來。一個 **CMSIS-DAP SWD 偵錯器 + USB-to-UART 橋接**，
跑在 RP2040（Debug Probe / Pico）與 RP2350（Pico 2）上，並加上**板上 OLED 即時診斷**與**跨廠牌 layer 2 目標支援**。

> 進度與分階段清單見 [`TODO.md`](TODO.md)。C 韌體架構剖析見 [`debugprobe/DEVELOP.md`](debugprobe/DEVELOP.md)。
> 壓測數據見 [`STRESS-test.md`](STRESS-test.md)。給 AI/開發者的架構與踩雷指引見 [`CLAUDE.md`](CLAUDE.md)。

## 功能現況（多數已實機驗證）

| 功能 | 狀態 |
|------|------|
| USB 列舉 + WinUSB（免驅動）+ flash 序號 | ✅ 實機 |
| SWD 物理層（PIO，RAW/SWDI 模式，弱驅動+慢 slew 抗長線振鈴） | ✅ 實機 |
| CMSIS-DAP 核心（從零重寫，SWD 全套 + posted read + WAIT 重試） | ✅ 實機 |
| **DAP v2 bulk（vendor）+ DAP v1 HID** 同一韌體並存 | ✅ 實機（probe-rs / OpenOCD / pyOCD） |
| UART 橋接（CDC-ACM，雙向 + 動態 baud） | ✅ 實機 |
| **AutoBaud**（PIO 量 RX 邊緣，魔術 baud 9728 觸發） | ✅ 實機 |
| **多核心 affinity**（OLED/LED→core1，core0 專責 USB/DAP/UART） | ✅ 實機 |
| RP2040 + RP2350 雙晶片、三板級 + binary_info | ✅ 實機 + UF2 |
| **OLED 診斷**：晶片偵測 / 可燒狀態 / SWD 邏輯波形 | ✅ 實機（見下） |
| **layer 2 跨廠牌目標**（RP2040 / STM32 / GD32） | ✅ 實機（見下） |

> 已用 **probe-rs**、**OpenOCD**（`cmsis-dap.cfg`+`rp2040.cfg`）、**pyOCD** 三套工具實機驗證偵錯與燒錄。

## OLED 板上診斷（接 SSD1306 0.96" I2C，選用）

探針在 **host 閒置時自主**用 SWD 偵測所接的 layer 2 目標，OLED 顯示：

- **晶片型號**：讀 CoreSight ROM table 的 JEP106 廠商碼跨廠牌辨識；ST/GD32 再讀 DBGMCU DEV_ID
  給精確型號（涵蓋 STM32 F0/F1/F2/F3/F4/F7/G0/G4/L0/L1/L4/L5/H7/WB/WL/U5/C0 約 55 種 +
  GD32、Nordic nRF、Raspberry Pi…）。
- **可燒狀態**：依目標 RDP 讀保護等級顯示 `flash OK (RDP0)` / `LOCK(RDP1)` / `RDP2` / `unknown`
  （F2/F4/F7 讀 OPTCR、F0/F1/F3 讀 OBR、L4/G0/G4 讀 OPTR）。
- **SWD 數位邏輯波形**：PIO0 SM1 + DMA 高速擷取 SWCLK/SWDIO，畫成 **2 通道方波**（邏輯示波器式），
  **token-ring 捲動**即時流動、拔線時平線捲入；擷取前固定 SWCLK 1MHz 使刻度一致，第 5 行顯示刻度。
- **不干擾偵錯**：有 host 連線時僅在閒置 >300ms 才插入擷取，probe-rs/openocd 偵錯期間不會被打斷。

> RP2040 的 GPIO 無法做真類比示波器（ADC 太慢且不在 SWD 腳），故為**數位**(1-bit)波形——
> 看得到邊緣/位元/時序/跨臨界毛刺，看不到類比振鈴。

## layer 2 跨廠牌目標支援

探針是通用 CMSIS-DAP SWD 探針，**與目標晶片無關 → 換目標不改探針核心**。已驗證/規劃：

| 目標 | 晶片 | 狀態 | 文件 |
|---|---|---|---|
| Raspberry Pi Pico | RP2040 | ✅ 實機 | `src/bin/uart*.rs` |
| WeAct Black Pill | STM32F401（矽晶 401xE） | ✅ 實機 | [`TEST-stm32f401.md`](TEST-stm32f401.md) |
| Nucleo-F446RE | STM32F446RET6 | ✅ 實機 | [`TEST-stm32f446re.md`](TEST-stm32f446re.md) |
| Anycubic Vyper / TriGorilla+ | GD32F103RET6 | 📋 接線計畫 | [`TEST-trigorilla-vyper.md`](TEST-trigorilla-vyper.md) |

接線、跳線插法、解讀保護（RDP unlock）流程與對等測試韌體總覽見 [`MULTI-TARGET.md`](MULTI-TARGET.md)。
STM32 目標韌體為獨立 crate（`stm32f401-target/`、`stm32f446-target/`，embassy-stm32）。

## 架構（C/FreeRTOS → Rust/Embassy）

FreeRTOS task → Embassy async task（USB 主迴圈、DAP 傳輸、UART 橋接、OLED/LED）。
SWD/AutoBaud 時序用 `embassy_rp::pio` + `pio_asm!`；CMSIS-DAP 指令解析全用 Rust 重寫。

```
src/
├── main.rs        進入點、中斷綁定、多核心、async task 編排、跨核狀態、binary info
├── board/         三板腳位/LED/UART/IO 設定（對應 include/board_*.h）
├── serial.rs      flash unique ID / OTP → USB 序號（get_serial.c）
├── probe/         SWD 物理層 PIO（probe.c + probe.pio）；長線 slew 軟化
├── dap/           CMSIS-DAP 核心（DAP.c + sw_dp_pio.c）+ 自主目標偵測（晶片/RDP）
├── logic.rs       SWD 數位邏輯擷取（PIO0 SM1 + DMA）→ OLED 波形
├── display.rs     SSD1306 OLED（晶片/可燒狀態/邏輯波形）
├── autobaud.rs    PIO 量 UART RX 邊緣推算 baud（autobaud.c）
├── usb/           device/描述符 + WinUSB + DAP vendor(v2) + HID(v1) + CDC
└── uart.rs        USB-CDC ↔ UART 橋接（cdc_uart.c）
```

## 建置與燒錄

需 Rust ≥ 1.85（edition 2024）。target 由 `rust-toolchain.toml` 自動安裝。

```bash
cargo build-probe    # board-debug-probe (RP2040) — 預設
cargo build-pico     # board-pico (RP2040, Pico 1)
cargo build-pico2    # board-pico2 (RP2350, Pico 2)
```

**一鍵建置+燒錄**（[`flash.sh`](flash.sh)，Windows 用 Git Bash）：

```bash
./flash.sh rp2040    # = pico：建置 board-pico → UF2 → picotool（需先 BOOTSEL）
./flash.sh probe     # board-debug-probe（需 BOOTSEL）
./flash.sh pico2     # Pico 2 / RP2350（需 BOOTSEL）
./flash.sh f401      # layer-2 STM32F401 目標（經探針 SWD 燒，免 BOOTSEL）
./flash.sh f446      # layer-2 STM32F446 目標（經探針 SWD 燒，免 BOOTSEL）
# PROBE_SERIAL=xxxx ./flash.sh rp2040   # 覆蓋探針序號
```

手動產生 UF2（RP2040 用 `elf2uf2-rs`、RP2350 用 `picotool`，family 不同）：

```bash
elf2uf2-rs target/thumbv6m-none-eabi/release/debugprobe-rs target/debugprobe.uf2          # RP2040
cp target/thumbv8m.main-none-eabihf/release/debugprobe-rs target/p2.elf && \
  picotool uf2 convert target/p2.elf target/debugprobe_on_pico2.uf2                        # RP2350
```

按住 BOOTSEL 接 USB 進 bootloader → 把 `.uf2` 拖入 RPI-RP2 磁碟（或 `picotool load -x`）。
探針本身（layer 1）只能用 BOOTSEL；layer 2 目標經探針用 SWD 燒（`probe-rs download` / `flash.sh f401`）。

## 與 C 版的主要差異

- **無 RTOS**：以 Embassy async/await 取代 FreeRTOS task。
- **記憶體安全**：DAP 解析與 SWD 傳輸以安全 Rust 重寫（無手動環形緩衝指標運算）。
- **除錯輸出**：`defmt` + RTT（取代 `probe_info`/printf）。
- **超出 C 版**：OLED 即時診斷（晶片偵測 / 可燒狀態 / SWD 邏輯波形）、跨廠牌 layer 2 支援、`flash.sh`。
