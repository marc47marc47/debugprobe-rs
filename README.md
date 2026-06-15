# debugprobe-rs

Raspberry Pi **Debug Probe** 韌體的 **Rust / Embassy** 重寫，從原始 C 韌體（`debugprobe/` 子目錄）逐步移植而來。提供 SWD（CMSIS-DAP v2）偵錯器與 USB-to-UART 橋接，可跑在 Debug Probe、Pico、Pico 2 上。

> 進度與分階段清單見 [`TODO.md`](TODO.md)。C 韌體的架構剖析見 [`debugprobe/DEVELOP.md`](debugprobe/DEVELOP.md)。

## 功能現況

| 功能 | 狀態 |
|------|------|
| USB 列舉 + WinUSB（免驅動）+ flash 序號 | ✅ 編譯通過 |
| SWD 物理層（PIO，RAW/SWDI 模式） | ✅ 編譯通過 |
| CMSIS-DAP 核心（從零重寫，SWD 全套 + posted read） | ✅ 編譯通過 |
| DAP v2 bulk 傳輸（vendor class） | ✅ 編譯通過 |
| UART 橋接（CDC-ACM，雙向 + 動態 baud） | ✅ 編譯通過 |
| RP2040 + RP2350 雙晶片、三板級 | ✅ 建置 + UF2 驗證 |
| DAP v1 HID / AutoBaud / 多核心 / break / 流控 | ⏸ 延後（見 TODO） |

> 注意：本機開發環境無實體硬體，故上述為**編譯/建置層級**驗證；實機行為（probe-rs 偵錯、序列埠收發）需燒錄後測試。UF2 產物已用 `picotool` 驗證 family 正確。

## 架構（C/FreeRTOS → Rust/Embassy）

5 條 FreeRTOS task → Embassy async task：USB 主迴圈、DAP v2 傳輸、UART 橋接、LED。
SWD/PIO 時序用 `pio-proc` + `embassy_rp::pio`；CMSIS-DAP 指令解析全用 Rust 重寫。

```
src/
├── main.rs        進入點、中斷綁定、async task 編排、binary info
├── board/         三板腳位/LED/UART/IO 設定（對應 include/board_*.h）
├── serial.rs      flash unique ID / OTP → USB 序號（get_serial.c）
├── probe/         SWD 物理層 PIO（probe.c + probe.pio）
├── dap/           CMSIS-DAP 核心（DAP.c + sw_dp_pio.c）
├── usb/           device/描述符 + WinUSB + DAP vendor + CDC（usb_descriptors.c）
└── uart.rs        USB-CDC ↔ UART 橋接（cdc_uart.c）
```

## 建置

需 Rust ≥ 1.85（edition 2024）。target 由 `rust-toolchain.toml` 自動安裝。

```bash
# Debug Probe（正式硬體，RP2040）— 預設
cargo build-probe        # = cargo build --release --no-default-features --features board-debug-probe

# Pico 1（RP2040）
cargo build-pico

# Pico 2（RP2350）
cargo build-pico2
```

## 產生 UF2 與燒錄

RP2040 用 `elf2uf2-rs`，RP2350 用 `picotool`（family 不同）：

```bash
# RP2040（Debug Probe / Pico）
cargo install elf2uf2-rs
elf2uf2-rs target/thumbv6m-none-eabi/release/debugprobe-rs target/debugprobe.uf2

# RP2350（Pico 2）— 需 .elf 副檔名
cp target/thumbv8m.main-none-eabihf/release/debugprobe-rs target/p2.elf
picotool uf2 convert target/p2.elf target/debugprobe_on_pico2.uf2
```

按住 BOOTSEL 接 USB 進入 bootloader，把對應 `.uf2` 拖入磁碟即可。
或用 SWD 燒錄器直接燒：`cargo run-probe`（`probe-rs` runner，已設定於 `.cargo/config.toml`）。

## 與 C 版的主要差異

- **無 RTOS**：以 Embassy async/await 取代 FreeRTOS task。
- **記憶體安全**：DAP 解析與 SWD 傳輸以安全 Rust 重寫（無手動環形緩衝指標運算）。
- **除錯輸出**：`defmt` + RTT（取代 `probe_info`/printf）。
- **延後項**：DAP v1 HID、AutoBaud、break/流控、多核心 affinity — 見 [`TODO.md`](TODO.md) 與其延後理由。
