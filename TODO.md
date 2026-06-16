# Debugprobe Rust 移植 TODO

把 `debugprobe/` 的 C 韌體（FreeRTOS + TinyUSB + Pico SDK + PIO）**從零用 Rust 重寫**，達成與 C 版**完整功能對等**。本檔為進度追蹤的單一事實來源；完成一項即把 `- [ ]` 改成 `- [x]`。

> **🏆 實機驗證里程碑（2026-06-15）**：Rust 韌體燒入 Pico 探針後，`probe-rs info`
> 成功偵錯真實 RP2040 目標，輸出與 C 基準逐字一致。USB 三介面（CMSIS-DAP v2 / CDC /
> 複合）列舉正常；板上 OLED 顯示 `USB: connected` 與 `DAP cmds: 702`（實際偵錯流量）。
> 核心功能（SWD 偵錯）已達成**實機對等**。

## 決策摘要

| 項目 | 決定 |
|---|---|
| 框架 | **Embassy**（`embassy-rp` + `embassy-usb` + `embassy-executor`），async task 取代 FreeRTOS |
| CMSIS-DAP 核心 | **從零用 Rust 重寫**（不綁 C 的 `DAP.c`、不用既有 crate） |
| 目標硬體 | RP2040（Debug Probe / Pico）+ RP2350（Pico 2），完整對等 |
| 範圍 | 最終完整對等，分階段逐步達成，每階段可獨立驗證 |
| 兩大里程碑 | **Phase 4**（DAP v2 實機偵錯）、**Phase 6**（UART 橋接） |

## 架構對應（C/FreeRTOS → Rust/Embassy）

| C 模組 / 機制 | Rust / Embassy 對應 |
|---|---|
| FreeRTOS task（usb/uart/dap/autobaud/wdog） | Embassy `#[embassy_executor::task]` async tasks |
| 雙核心 affinity（core0 USB/UART、core1 DAP/AB） | `embassy_rp::multicore::spawn_core1` + 兩個 executor |
| TinyUSB device stack | `embassy-usb`（`UsbDevice::run()`） |
| CDC-ACM（UART 橋接） | `embassy_usb::class::cdc_acm::CdcAcmClass` |
| DAP v2 自訂 vendor class + bulk EP | embassy-usb Builder 自訂 interface/endpoints |
| BOS + MS OS 2.0（WinUSB driverless） | `embassy_usb::msos` 模組 |
| `probe.c` / `probe.pio` / `probe_oen.pio` | `embassy_rp::pio` + `pio_proc::pio_asm!` |
| `sw_dp_pio.c`（SWD_Transfer 等） | Rust SWD 後端（呼叫 PIO 包裝層） |
| `CMSIS_DAP/DAP.c`（指令解析核心） | 從零重寫的 `dap` 模組 |
| `autobaud.c` + `autobaud.pio` + 雙 DMA | `embassy_rp::pio` + `embassy_rp::dma` ring buffer |
| `get_serial.c`（flash unique ID） | `embassy_rp` unique id → USB serial |
| SOF 看門狗（RP2040） | async task 讀 USB SOF 暫存器；逾時重啟 USB |
| `bi_decl`（binary info） | `embassy_rp::binary_info`（picotool 可讀） |
| `probe_info`/printf 除錯 | `defmt` + `defmt-rtt` |
| CMake 板級巨集 / `DEBUG_ON_PICO` | Cargo features（`board-*` 與 `rp2040`/`rp2350`） |

---

## Phase 0 — 專案骨架與工具鏈 ✅
- [x] 將 `Cargo.toml` 改為 `no_std` 嵌入式設定
- [x] 加入依賴：`embassy-executor`、`embassy-rp`、`embassy-usb`、`embassy-time`、`embassy-sync`、`embassy-futures`、`defmt`、`defmt-rtt`、`panic-probe`、`pio`、`pio-proc`、`static_cell`、`portable-atomic`、`heapless`、`cortex-m(-rt)`、`critical-section`
- [x] 建立 `.cargo/config.toml`（target `thumbv6m-none-eabi` / `thumbv8m.main-none-eabihf`、runner `probe-rs run`）
- [x] 建立 `rust-toolchain.toml`、`build.rs`（依 feature 選 memory.x）
- [x] 建立 RP2040 / RP2350 各自的 `memory.x`（RP2350 含 `.start_block`/`.end_block`/`.bi_entries` section）
- [x] 定義 Cargo features：晶片（`rp2040`/`rp2350`）與板級（`board-debug-probe`/`board-pico`/`board-pico2`）
- [x] 建立 board config 模組（`src/board/`，對應 `include/board_*.h` 腳位/LED/UART/IO 模式）
- [x] 用 `embassy-rp` 寫最小 `#[embassy_executor::main]`，閃爍 USB-connected LED
- [x] **驗證**：三套組合皆 `cargo build` 通過（RP2040 debug-probe / RP2040 pico / RP2350 pico2）；LED 閃爍待實機

> 備註：embassy-executor 0.10 的 arch feature 為 `platform-cortex-m`（非 `arch-cortex-m`）。
> RP2040 boot2 與 RP2350 image def 皆由 embassy-rp 自動提供；RP2350 不可傳 `-Tlink-rp.x`。

## Phase 1 — USB 裝置列舉與描述符 ✅（compile）
- [x] 以 `embassy-usb` 建立 device descriptor（VID `0x2E8A`、PID `0x000C`、bcdDevice `0x0231`）— `src/usb/mod.rs`
- [x] 字串描述符（製造商 "Raspberry Pi"、產品字串依板級）
- [x] 序號取自 flash unique ID / OTP chip id（`src/serial.rs`，移植 `get_serial.c`；RP2040 用 flash、RP2350 用 OTP）
- [x] 以 `embassy_usb::msos` 加入 BOS + MS OS 2.0 表頭（vendor code 1），GUID `{CDB3B5AD-293B-4663-AA36-1AAE46463776}` 保留
- [x] spawn USB run task（對應 C 的 usb_thread / tud_task）
- [x] **驗證（compile）**：三板皆 `cargo build` 通過；實機列舉 / Windows WinUSB 待硬體
- [ ] （延至 Phase 4）function-level WinUSB CompatibleId "WINUSB" + DeviceInterfaceGUIDs 隨 DAP vendor interface 加入

## Phase 2 — SWD 物理層（PIO）✅（compile）
- [x] 用 `pio::pio_asm!` 移植 `probe.pio`（RAW/SWDI）— `src/probe/mod.rs`（重排成 get_next_cmd 為 origin）
- [x] 移植 `probe.c`：`set_swclk_freq`（clock divider，向上取整）
- [x] 移植 `write_bits`/`read_bits`/`hiz_clocks`/`read_mode`/`write_mode`（async，用 PIO FIFO wait_push/wait_pull）
- [x] 移植 `init`(new)/`deinit`/`assert_reset`/`reset_level`（含 open-drain reset 模擬）
- [x] RAW（in=SWDIO）與 SWDI（in=獨立腳）模式，依板級 swdi 腳選擇；wiring 進 main（PIO0/SM0）
- [x] **驗證（compile）**：三板皆建置通過；實機 SWCLK/SWDIO 時序待邏輯分析儀
- [ ] （延後）`probe_oen.pio`（OEN 模式）— 無內建板使用，待有 OEN 硬體時補上

## Phase 3 — CMSIS-DAP 核心（從零重寫）✅（compile）
- [x] 新增 `dap` 模組（`src/dap/mod.rs`）：DAP 指令 ID 與封包框架
- [x] 一般指令：`DAP_Info`、`HostStatus`、`Connect`/`Disconnect`、`Delay`、`ResetTarget`
- [x] 傳輸設定：`TransferConfigure`、`SWD_Configure`
- [x] SWJ：`SWJ_Pins`（nRESET）、`SWJ_Clock`、`SWJ_Sequence`
- [x] 序列：`SWD_Sequence`
- [x] 傳輸：`Transfer`（含 posted AP read）、`TransferBlock`、`TransferAbort`、`WriteABORT`
- [x] 重寫 SWD 傳輸邏輯（對應 `sw_dp_pio.c`）：請求封包、ACK、讀/寫資料相 + parity、WAIT/FAULT/協定錯誤、idle cycles、WAIT 重試
- [x] 能力宣告對齊 `DAP_config.h`（SWD on、JTAG off、SWO off、`PACKET_SIZE=64`、`PACKET_COUNT=8`）
- [x] **驗證（compile）**：RP2040 建置通過
- [ ] （延後）`QueueCommands`/`ExecuteCommands` 聚合 → Phase 4 task 框架處理
- [ ] （延後）value-match / match-mask / timestamp；host 端單元測試需抽 SWD backend trait
- [ ] （延後）`probe_set_swclk_freq` 由 DAP `clock_delay` 推導（C `MAKE_KHZ`）— 目前用 SWJ_Clock 直接設定

## Phase 4 — DAP v2 USB 傳輸（vendor class + bulk）★里程碑 ✅（compile）
- [x] 以 embassy-usb Builder 建立自訂 vendor interface（class 0xFF）— `src/usb/mod.rs`
- [x] 建立 bulk IN/OUT endpoints（對應 `tusb_edpt_handler.c`）
- [x] function-level WinUSB CompatibleId "WINUSB" + DeviceInterfaceGUIDs（補完 Phase 1 延後項）
- [x] DAP async task：讀 bulk OUT → `dap.execute_command` → 寫 bulk IN（`dap_task` in `main.rs`）
- [x] **驗證（compile）**：三板皆建置通過
- [x] **驗證（實機）✅**：Rust 韌體燒入 Pico 探針，`probe-rs info --chip RP2040 --protocol swd` 成功讀到目標 RP2040 的 DPv2/MINDP、兩個 multidrop MemoryAP 與 ROM Table（0xe00ff000），輸出與 C 基準**逐字一致**
- [x] **關鍵 bug 修正**：`composite_with_iads = true` 時 embassy 要求 device class `0xEF/0x02/0x01`，原本設 0x00 導致 `Builder::new` panic、USB 從未列舉（用板上 OLED 逐階段標記定位）
- [ ] （延後）Atomic/Queued commands 聚合（C 的環形雙緩衝最佳化）；目前每封包一命令

## Phase 5 — DAP v1（HID）傳輸 ✅ 實機驗證
> 改良 C 的編譯期二選一 → **同時提供 v1 HID + v2 bulk**（`src/usb/mod.rs` 加 HID 介面，
> `dap_task` 用 `select` 服務兩種來源）。一個 build 同時支援 probe-rs/OpenOCD(v2) 與
> pyOCD/HID 工具(v1)。
- [x] 加入 HID in/out 傳輸（vendor I/O report descriptor，64-byte report）
- [x] dap_task 以 `select` 同時處理 v2 bulk 與 v1 HID（共用同一 Dap 核心）
- [x] **驗證（實機）✅**：`pyocd list` 找到探針（`Raspberry Pi Debugprobe on Pico`）；
      v2(probe-rs/OpenOCD) 無退化
- 註：pyOCD 先前找不到是因 Windows 上 v2 需 libusb 後端；加 v1 HID 後改走 hidapi 即可

## 額外 — OLED 活動顯示（layer 1）✅
- [x] A 的 OLED 顯示最後收到的 host DAP 指令名稱 + 累計數、UART(client log) 收發位元組
- [x] 用非阻塞 atomic 捕捉（LAST_DAP_CMD/UART_RX_BYTES/UART_TX_BYTES），OLED 僅在 DAP 閒置時 flush
- [x] 兩板 LED 心跳（A=GP25、B=GP25）；B 的 uarthello/uartmon 皆顯示 OLED 狀態

## Phase 6 — UART 橋接（CDC-ACM）★里程碑 ✅（compile）
- [x] 以 `CdcAcmClass` + `embassy_rp::uart::BufferedUart`（UART1, GPIO4/5）建立雙向橋接 task — `src/uart.rs`
- [x] 雙向資料：UART RX→USB、USB→UART TX（select 只取消讀取，寫入保證完成 = cancel-safe）
- [x] line coding 動態 baud rate（`control_changed` → `set_baudrate`）
- [x] **驗證（實機）✅ UART→USB**：Rust 探針 SWD 燒 `uarthello`(src/bin) 到目標，目標 UART0 輸出
      經橋接 → COM11 收到 `hello from target #740...`
- [x] **驗證（實機）✅ USB→UART**：目標燒 `uartmon`(src/bin，讀 RX 顯示到 layer2 OLED + echo)，
      host 寫 COM11 → 橋接 → 目標 B.RX，B 的 OLED 顯示收到字串、並 echo 回 COM11（21/24 bytes roundtrip）
      → **雙向橋接皆實機證實**；layer 2 也接了 OLED 可獨立 debug
- [ ] （延後）data bits / parity / stop bits 執行期切換（embassy BufferedUart 僅暴露 set_baudrate；目前 8N1）
- [ ] （延後）break（含定時 break、CAP_BREAK descriptor）
- [ ] （延後）HWFC / 軟體流控（RTS/CTS/DTR GPIO）
- [ ] （延後）TX/RX LED（含 debounce）與 overflow 計數

## Phase 7 — AutoBaud（PIO1 邊緣計數）✅ 實機驗證
> **接腳共享解法**：用 **PIO1** 透過 `embassy_rp::pac`（unstable-pac feature）直接設
> SM 的 `pinctrl.in_base` / `execctrl.jmp_pin = GP5`，**不呼叫 make_pio_pin**（不更動
> funcsel），因此 UART1 仍擁有 GP5、PIO1 只讀其輸入同步器（RP2040 GPIO 輸入對所有
> 周邊永遠可見）。`src/autobaud.rs`。
- [x] 移植 `autobaud.pio`（邊緣間隔計數，PIO1）
- [x] 軟體讀 FIFO（取代 C 的雙 DMA）+ 「最短且重複出現間隔 = 1 bit time」估算
- [x] 魔術 baud `9728` 觸發（`uart.rs` bridge 的 control_changed → `autobaud.detect()`）
- [x] **驗證（實機）✅**：目標 B 跑 uarthello 以 115200 連續發送；主機 COM 設 9600 → 亂碼(3F 3F)，
      切 9728 → AutoBaud 偵測出 115200 → 讀到清晰 `hello from target #87..90`
- [x] **附帶修正**：A 的 OLED blocking I2C flush 會卡 executor 間歇打斷 DAP →
      改為「DAP 活動中跳過 OLED flush」+ I2C 400kHz（`main.rs` 狀態迴圈）

## 額外 — 板上 OLED 狀態顯示（實機新增）✅
- [x] SSD1306 128x64 I2C（I2C1: SCL=GP7, SDA=GP6）`src/display.rs`
- [x] BufferedGraphicsMode + embedded-graphics 文字（TerminalMode 在此面板只顯示末字元，故改用 framebuffer）
- [x] 狀態畫面：產品名、序號、USB 連線狀態、DAP 指令計數（跨 task atomics）
- [x] 開發過程用單字元階段標記定位 `Builder::new` panic（device class bug）
- [ ] （選用）顯示目標 IDCODE / UART 收發位元組數

## Phase 8 補完 — binary_info ✅ / 多核心 ❌（嘗試後撤回）
- [x] **binary_info picotool 顯示修正** ✅：根因是 picotool 在 rp2040 只掃 boot2 後前 256B
      找 header，原本 `.boot_info` 放在 `.text` 之後掃不到。依 rp-binary-info 文件改放
      `INSERT AFTER .vector_table` + `_stext = ADDR(.boot_info)+SIZEOF(.boot_info)`
      （`memory_rp2040.x`）。`picotool info` 現顯示 name/version/description；probe-rs 無退化。
- [x] **多核心 affinity（後來達成，見下方 MC-2）** —— 初次嘗試失敗紀錄如下，真因實為
      「core0 main task 返回」而非 core1；於 MC-2 修正後成功。以下保留初次誤判分析供參。
- [ ] ~~多核心 affinity（初次嘗試後撤回，誤判為無法實現）~~
  - **嘗試**：`spawn_core1` 把 OLED+LED 狀態迴圈搬到 core1，core0 留 USB/DAP/UART/AutoBaud
    （對應 C 的 `vTaskCoreAffinitySet`）。編譯通過、DebugOled/Output 皆 Send。
  - **故障（實機、一致可重現）**：USB 列舉正常、probe-rs 偵測到探針、**DP IDCODE(DPIDR) 可讀**，
    但 **AP(Access Port) 存取一致失敗**（"error in communication with access port or debug port"，
    100kHz 降速亦同）。撤回單核後立即恢復 → 確認為多核心所致。
  - **推測原因**（未深入定位，屬 optional 故不續查）：
    1. RP2040 thumbv6m 上 `critical-section-impl` 與 `portable-atomic` 皆用**跨核全域硬體
       spinlock**；兩核同時頻繁取臨界區（core0：SWD 傳輸+atomic+embassy-time；core1：OLED
       blocking I2C+atomic+embassy-time）造成競爭，可能在 AP 多階段傳輸的關鍵時刻 stall core0。
    2. USB/PIO 中斷綁在 core0；把 USB 相依的 DAP pipeline 跨核切分，與 embassy「周邊由單一
       executor 擁有、中斷在單核」的模型不合。DP 單筆讀成功、AP 的 posted-read + RDBUFF +
       WAIT-retry 多階段序列較敏感而失敗。
  - **結論**：C 能做 affinity 是因 FreeRTOS+TinyUSB 有為跨核設計的顯式鎖；embassy 的型別安全
    單-executor 模型不適合切分 USB-coupled 的 DAP。且**單核已足**：M0+ @125MHz async 協作排程
    已能順暢交錯 USB/DAP/UART/OLED（唯一阻塞的 OLED I2C 用「DAP 活動跳過」處理），
    probe-rs/OpenOCD/pyOCD + AutoBaud + UART 橋接全部單核驗證通過。多核心為純效能優化，非功能需求。

## 批次化 OLED + 重做多核心（MC 系列）
> OLED 改每 3s 批次（不即時逐事件）；版面拿掉標題，改成 pg version + 最近數筆事件 + rx/tx；
> 事件用 token ring（環形緩衝，最新覆蓋最舊、無溢出）。在此基礎上重做多核心，
> 並推測先前失敗真因為 core1 stack 溢位（放大到 32KB 排除）。詳見 plan 檔。

### Phase MC-1 — 批次化 OLED + token ring（單核，低風險，獨立可用）✅
- [x] token ring：`EVT_RING: [AtomicU8; 3]` + `EVT_SEQ: AtomicU32`；dap_task 單一寫入（`record_evt`）；移除 `LAST_DAP_CMD`/`USB_UP`/`DAP_COMMANDS`
- [x] OLED 內容：`debugprobe-rs <ver>`(env!) + 最近 3 筆事件(`evt_name`/cmd_name) + `rx:n tx:m`（沿用 display.rs 排版）
- [x] OLED 迴圈改 3s 批次（core0），移除 `dap_active` 跳過；LED 3s 心跳
- [x] **驗證**：probe-rs AP 存取正常（MemoryAP+ROM，單核基準）；OLED 新版面待使用者目視確認

### Phase MC-2 — 重做多核心（OLED→core1）✅ 成功
- [x] `spawn_core1` + 第二 `Executor` + `CORE1_STACK=32KB`；批次 OLED task 移 core1
- [x] core0 留 USB/DAP/UART/AutoBaud
- [x] **真因修正（關鍵）**：core0 的 `main` task **不可返回**！spawn_core1 後讓 main 返回會使
      core0 DAP **AP 存取一致失敗**（DP 可讀、AP 壞）；`main` 結尾加 `core::future::pending().await`
      park 住即解決。先前誤判為「core1 本身」，實為 embassy main task 返回所致。
- [x] **驗證（硬性閘通過）**：`probe-rs info` 顯示 MemoryAP + ROM 0xe00ff000（一致，重試皆過）；
      OpenOCD 雙核 examination 成功；三板建置 + clippy 零警告。OLED core1 3s 更新待目視確認。

### Phase MC-3 — 結果處置與提交
- [ ] 成功 → 多核心達成，TODO 標 ✅，commit + tag v0.3.1
- [ ] 失敗（AP 仍壞）→ 回退單核（保留 MC-1 批次 OLED），TODO 記錄根因，commit + tag v0.3.1

## 極限效能 / 壓力測試（ST 系列）✅ 完成 — 詳見 `STRESS-test.md`
- [x] ST-0：B 燒 `src/bin/uartecho.rs`（UART0 全速 echo，baud 為 const）
- [x] ST-1 SWD：最高穩定 **15.6MHz**（31.25MHz 失敗）；讀 **~94 KiB/s**、寫 **~203 KiB/s**；瓶頸 USB-FS+DAP
- [x] ST-2 UART：最高無遺失 **2.5 Mbaud**（3M 掉資料）；峰值 **~209 KB/s** round-trip
- [x] ST-3 soak：30×128KB 連讀 **30/30、0 錯誤、3.84MB**，事後 AP 正常；多核心重載穩定
- [x] ST-4 DAP 速率：單筆 **~3.6k transfers/s**（USB 往返限制）；block ~24k(讀)/52k(寫) words/s
- [x] ST-5：彙整 `STRESS-test.md`（最大負荷一覽 + 瓶頸分析）

## Phase 8 — 系統穩定性 / 多核心 / 省電（部分完成）
- [x] panic / 錯誤處理（`panic-probe` + `defmt`；no_alloc 故無 malloc hook）
- [x] `binary_info`（程式名/描述/版本，`src/main.rs`；對應 C `bi_decl`）— 三板皆建置；
      ⚠ picotool 顯示待解（rp2040 root header 機制與 rp-binary-info `.boot_info` 不符）
- [x] USB suspend/resume / bus-reset：由 `embassy-usb` 的 `UsbDevice::run()` 原生處理
      （C 的 SOF 看門狗 `dev_mon` 為 RP2040 特定 workaround，embassy 事件處理已涵蓋）
- [ ] （延後）多核心 affinity（`spawn_core1`：DAP 移至 core1）— 屬效能最佳化，
      單核心功能已完整；需實機驗證無死鎖，暫緩
- [ ] （延後）SOF 看門狗顯式實作（如實機發現 embassy 原生處理不足再補）

## Phase 9 — 多板級 / RP2350 對等 與建置產物 ✅
- [x] `board-debug-probe`（腳位 12-14、UART1 4/5、多 LED、無 reset）
- [x] `board-pico`（腳位 2/3、reset GPIO1、LED 25）
- [x] `board-pico2`（RP2350）
- [x] RP2040 與 RP2350 雙晶片建置（各自 `memory.x`、target、feature gate）
- [x] cargo 別名 `build-probe`/`build-pico`/`build-pico2`/`run-probe`（`.cargo/config.toml`）
- [x] **UF2 已驗證**：`debugprobe.uf2`(rp2040) / `debugprobe_on_pico.uf2`(rp2040) /
      `debugprobe_on_pico2.uf2`(rp2350-arm-s)，family 由 `picotool info` 確認；命名對齊 C 版
      （RP2040 用 `elf2uf2-rs`、RP2350 用 `picotool uf2 convert`）

## Phase 10 — 整合驗證與文件（大致完成）
- [x] 撰寫根目錄 `README.md`（總覽 / 建置 / UF2 / 燒錄 / 與 C 差異 / 現況）+ `TEST-plan.html`（接線圖）
- [x] 與 C 版功能比對：核心（SWD 偵錯 + UART 橋接 + AutoBaud）對等，延後項已列明
- [x] **測試矩陣（實機）✅**：`probe-rs info`/`download` 與 **OpenOCD**（`cmsis-dap.cfg` + `rp2040.cfg`）
      皆成功 —— OpenOCD 認得 CMSIS-DAPv2、讀 DP IDCODE、**雙核 Cortex-M0+ examination 成功**、開 GDB server
- [x] **Windows driverless ✅**：列舉為 WinUSB CMSIS-DAP v2，無需手動安裝驅動
- [ ] （選用）pyOCD 測試（與 OpenOCD 類似，預期可用）

---

## Phase 11 — layer 2 多晶片支援（探針驗證跨廠牌目標）

探針本身是通用 CMSIS-DAP SWD 探針、與目標晶片無關，故**不改探針韌體**；本階段驗證探針能把
**非 RP2040** 的晶片當作 layer 2 目標，並提供對等的目標測試韌體（OLED + LED + UART rx/tx）。
詳見 [`MULTI-TARGET.md`](MULTI-TARGET.md)。

- [x] **WeAct Black Pill（STM32F401CCU6；矽晶實為 401xE/512KB, Cortex-M4F）**：新增獨立 crate
      `stm32f401-target/`（embassy-stm32 0.6，target `thumbv7em-none-eabihf`，與根套件解耦）
- [x] 目標韌體 `stm32f401-target/src/main.rs`：板載 LED(PC13) 閃爍 + OLED(I2C1 PB8/PB9) 狀態
      + USART1(PA9/PA10) BufferedUart 週期 TX 心跳 + RX echo（鏡像 RP2040 的 uartmon/uarthello）
- [x] `cargo build --release` 編譯通過（critical-section 由 `cortex-m/critical-section-single-core` 提供）
- [x] 探針 SWD 長線訊號完整性：`src/probe/mod.rs` 把 SWCLK/SWDIO 改 2mA 弱驅動 + 慢 slew + 輸入 Schmitt
- [x] **實機驗證 ✅（2026-06-16）**：清除出廠 RDP Level 1（OpenOCD `stm32f2x unlock`，mass erase）後，
      探針 A 經 `probe-rs download`/`reset` 燒錄成功、LED(PC13) 閃、UART **雙向**皆通。詳見 `MULTI-TARGET.md`

### Nucleo-F446RE（STM32F446RET6, Cortex-M4F, 512KB/128KB）
- [x] 新增獨立 crate `stm32f446-target/`（鏡像 F401；feature `stm32f446re`、LED PA5、chip `STM32F446RETx`）
- [x] `cargo build --release` 編譯通過
- [x] 接線計畫文件 `TEST-stm32f446re.md` / `.html`（接線、跳線插法〔移除 CN2〕、OLED 接法）
- [x] **實機驗證 ✅（2026-06-16）**：移除 CN2 後，清 RDP Level 1（本顆有）→ 因 NRST 與 ST-LINK 共線，
      reset-based 燒錄 timeout，改用 OpenOCD `halt + flash write_image + SYSRESETREQ` 燒錄成功、UART **雙向**皆通

---

## 關鍵檔案（將新增 / 修改）

- `Cargo.toml`（根目錄）— 改為 embedded no_std，加入 embassy 依賴與 features
- `.cargo/config.toml`、`rust-toolchain.toml`、`build.rs`、`memory_rp2040.x` / `memory_rp2350.x`
- `src/main.rs` — `#[embassy_executor::main]` 入口（取代 hello world）
- `src/board/` — 板級設定，對應 `debugprobe/include/board_*.h`
- `src/probe/` — SWD PIO 物理層，對應 `probe.c`/`probe*.pio`/`sw_dp_pio.c`
- `src/dap/` — 從零重寫的 CMSIS-DAP 核心，對應 `CMSIS_DAP/DAP.c`
- `src/usb/` — device/descriptors/msos + vendor(DAP) + cdc(UART)
- `src/uart.rs`、`src/autobaud.rs`、`src/serial.rs` — 對應 `cdc_uart.c`/`autobaud.c`/`get_serial.c`
- 參考來源：`debugprobe/`（C 實作）與 `debugprobe/DEVELOP.md`（架構剖析）

## 風險 / 注意事項

- `Cargo.toml` 目前 `edition = "2024"`（Rust 1.85+ 已穩定，可沿用）。
- 自訂 vendor class 在 embassy-usb 需手動組 interface/endpoint descriptor — Phase 4 主要技術風險。
- RP2350（thumbv8m）與 RP2040（thumbv6m）的 target / HAL feature 差異需以 feature gate 處理。
- 從零重寫 DAP 工作量大；Phase 3 先以 host 端單元測試驗證 parser 再上機。
