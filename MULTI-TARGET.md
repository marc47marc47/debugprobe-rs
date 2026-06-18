# layer 2 多晶片目標支援

debugprobe-rs 探針是**通用 CMSIS-DAP SWD 探針**,與目標晶片無關 → 把任意 SWD 目標當 layer 2
**完全不需改探針韌體**(唯一例外是長線訊號完整性的 slew 微調,見下)。本文件記錄非 RP2040
目標的接線、燒錄與對等測試韌體。

| 目標 | 晶片 | crate / 韌體 | 狀態 |
|---|---|---|---|
| Raspberry Pi Pico | RP2040 (Cortex-M0+) | `src/bin/uarthello\|uartmon\|uartecho.rs` | ✅ 實機驗證 |
| WeAct Black Pill | STM32F401CCU6（矽晶實為 401xE/512KB）(Cortex-M4F) | `stm32f401-target/` | ✅ **實機驗證**（2026-06-16）|
| Nucleo-F446RE | STM32F446RET6 (Cortex-M4F, 512KB/128KB) | `stm32f446-target/` | ✅ **實機驗證**（2026-06-16）；接線/流程見 [`TEST-stm32f446re.md`](TEST-stm32f446re.md) |
| Blue Pill | STM32F103C8T6 (Cortex-M3, 64KB/20KB) | `stm32f103-target/` | 🟡 韌體完成、待實機驗證；腳位/功能同 F401 |

---

## STM32F401CCU6 "Black Pill"（WeAct）

> 實測重點:這顆雖標示 **CCU6**,但 SWD 讀回 `device id = 0x10016433` / **flash 512 KiB** /
> **SRAM 96KB**,即矽晶是 **STM32F401xE**。出廠**開著 RDP Level 1（讀保護）**,且 Black Pill
> **無板載偵錯器**(USB-C 供電)。

### 接線:探針 A（Pico, board-pico）→ Black Pill

| 探針 A | Black Pill | 備註 |
|---|---|---|
| GP2 SWCLK | SWCLK (PA14) | 4-pin SWD 排針 |
| GP3 SWDIO | SWDIO (PA13) | 4-pin SWD 排針 |
| GND | GND | **就近、獨立一條短地線**（緊貼 SWDIO） |
| GP1 (nRESET) | NRST | 解 RDP / connect-under-reset 必需 |
| GP4 (UART1 TX) | PA10 (USART1 RX) | |
| GP5 (UART1 RX) | PA9 (USART1 TX) | |
| — | OLED SCL=PB8 / SDA=PB9 | VCC=3V3、GND |
| — | LED = PC13（板載,active-low）| 免接線 |
| — | USB-C | **Black Pill 自身供電**(確認 3V3 軌穩) |

### 測試韌體（`stm32f401-target/src/main.rs`）

對等於 RP2040 的 `uartmon`+`uarthello`:LED(PC13) 閃爍、OLED(I2C1 PB8/PB9, 400kHz) 顯示
`f401 layer2` / 最後 RX 文字 / `tx{n} rx{total}`;USART1 @115200 BufferedUart `select` 在 RX 與
1s timer 間切換 —— 逾時送 `hello from f401 #n\r\n`(TX),收到 RX 則 echo 回(反向橋接)。

### 燒錄流程（出廠 RDP Level 1 → 必須先解保護）

```bash
# 0. 建置（獨立 crate；feature=stm32f401cc、target thumbv7em-none-eabihf）
cd stm32f401-target && cargo build --release

# 1. 清除 RDP（connect-under-reset；RDP 1→0 觸發 mass erase；杜邦線用低速）
#    （需 OpenOCD；用其 cmsis-dap.cfg + stm32f4x.cfg）
openocd -f interface/cmsis-dap.cfg -c "transport select swd" -c "adapter speed 1000" \
  -f target/stm32f4x.cfg -c "reset_config srst_only srst_nogate connect_assert_srst" \
  -c "init" -c "catch { reset halt }" -c "catch { halt }" -c "stm32f2x unlock 0" \
  -c "reset run" -c "shutdown"
#    → 印出 "stm32f2x unlocked." 後，【拔插 USB-C 重新上電】讓 option byte 重載 + 完成抹除

# 2. 確認 RDP 已歸零（不重置也能完整讀 AHB / ROM table）
probe-rs info --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000

# 3. SWD 燒錄 + 重置
probe-rs download --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000 \
  target/thumbv7em-none-eabihf/release/stm32f401-target
probe-rs reset --chip STM32F401CCUx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd

# 4. 驗證 UART（探針 CDC = COMx，@115200）：收到 "hello from f401 #n"；打字會被 echo 回
```

### 踩雷與關鍵（**實測花最多時間的部分**）

1. **供電**:Black Pill 必須用自己的 USB-C 供電。只靠 SWD/NRST 腳寄生供電(~2V)時,DP/AP
   暫存器讀得到、但 AHB 匯流排存取一律失敗,徵狀極似 RDP/訊號問題,易誤判。
2. **RDP Level 1**:出廠開讀保護 → flash/SRAM 的 debug 存取被硬體封鎖,`probe-rs download` 直接
   連線失敗。必須先 `stm32f2x unlock 0`(會 **mass erase**,清掉出廠韌體),再上電生效。
3. **connect-under-reset**:出廠韌體會進 sleep(關 HCLK → AHB 無限 WAIT),且 RDP 下需在 reset
   釋放前 halt。故解 RDP 與初次連線都要 `--connect-under-reset` / `connect_assert_srst`。
4. **SWD 長線訊號完整性**:杜邦線太長時,DP(單筆)永遠 OK 但 AP/AHB(多次 turnaround)時好時壞、
   報 `Protocol error` / `did not respond`。**降 SWCLK 頻率無效**(頻率不影響 PIO 邊緣速率)。
   解法兩路並用:① **SWCLK/SWDIO/GND 用最短線、就近接地**;② 探針韌體把 SWCLK/SWDIO 改
   **2mA 弱驅動 + 慢 slew + 輸入 Schmitt**(`src/probe/mod.rs`,軟化邊緣壓反射)→ 重燒探針 A。
5. **快速開關 CDC COM 掃 baud** 會把探針 USB 堆疊弄到瞬斷(會自行重列舉);驗 UART 時單次開埠即可。
6. 晶片名用 `STM32F401CCUx`(或 `STM32F401CEUx`);LED 在 **PC13**(非 Nucleo 的 PA5);
   feature 用 `stm32f401cc`。

### 實作要點（crate 結構）

- **獨立 crate**:F401 是不同 target(`thumbv7em-none-eabihf`),子目錄 `Cargo.toml` 加空 `[workspace]`
  與根解耦;子 `.cargo/config.toml` 的 `build.target` 覆蓋根的 thumbv6m。
- **critical-section**:embassy-stm32 不自帶 → `cortex-m` 開 `critical-section-single-core`(單核)。
- **BufferedUart 中斷**:`bind_interrupts!` 綁 `BufferedInterruptHandler<USART1>`(非 `InterruptHandler`)。
- **memory.x** 由 embassy-stm32 `memory-x` feature 自動產生,**勿** link-rp.x。
- **I2C 頻率** 在 `i2c::Config.frequency`(非建構子參數)。

---

## Nucleo-F446RE（STM32F446RET6）

完整接線/跳線/OLED 計畫見 **[`TEST-stm32f446re.md`](TEST-stm32f446re.md)**（+ `.html` 向量圖）。
crate `stm32f446-target/` 鏡像 `stm32f401-target/`,僅差異:embassy-stm32 feature `stm32f446re`、
LED 改 **PA5**(Nucleo LD2, active-high)、runner/chip `STM32F446RETx`;USART1(PA9/PA10)、I2C1(PB8/PB9) 不變。

**與 Black Pill 最大不同**:Nucleo **有板載 ST-LINK** → **務必先移除 CN2 兩顆跳線**讓外部探針獨佔 SWD
(否則 ST-LINK 爭線,徵狀同「DP OK / AHB 失敗」)。UART 避開 PA2/PA3(ST-LINK VCP)→ 用 USART1。

**實測（2026-06-16）兩個 Nucleo 專屬眉角**:
1. 本顆 F446 **帶 RDP Level 1**(Nucleo 少見但確有)→ 先 `stm32f2x unlock 0`(mass erase)+ 拔插上電。
2. **NRST 與板載 ST-LINK 共線** → 任何走 reset 的燒錄(`probe-rs download`、OpenOCD `program`)都卡在
   「halt 後 reset timeout」。但**純 `halt` 成功**(DHCSR S_HALT=1)。可靠流程:`halt → flash write_image
   erase → SYSRESETREQ run`(完整指令見 [`TEST-stm32f446re.md`](TEST-stm32f446re.md) §4b)。
   若你的 Nucleo NRST 無共線干擾,`probe-rs download --chip STM32F446RETx` 可直接用。

---

## STM32F103C8 "Blue Pill"

最便宜常見的 layer-2 目標。crate `stm32f103-target/` **鏡像 `stm32f401-target/`,腳位與功能完全一致**,
只差晶片本身:F103 是 **Cortex-M3(無 FPU)** → target `thumbv7m-none-eabi`、embassy-stm32 feature
`stm32f103c8`、runner/chip `STM32F103C8Tx`。OLED 自動偵測認得它(DBGMCU DEV_ID `0x410` → 顯示 `STM32F1/GD32`)。

### 接線:探針 A（Pico, board-pico）→ Blue Pill（與 F401 同腳位）

| 探針 A | Blue Pill | 備註 |
|---|---|---|
| GP2 SWCLK | SWCLK (PA14) | 板邊 4-pin SWD 排針 |
| GP3 SWDIO | SWDIO (PA13) | 板邊 4-pin SWD 排針 |
| GND | GND | **就近短地線**（緊貼 SWDIO） |
| GP1 (nRESET) | R(NRST) | 解 RDP / connect-under-reset 用 |
| GP4 (UART1 TX) | PA10 (USART1 RX) | |
| GP5 (UART1 RX) | PA9 (USART1 TX) | |
| — | OLED SCL=PB8 / SDA=PB9 | VCC=3V3、GND（PB8/PB9 走 I2C1 重映射，embassy 自動）|
| — | LED = PC13（板載,active-low）| 免接線 |
| — | USB / 3V3 | Blue Pill 自身供電（勿只靠 SWD 腳寄生供電）|

### 燒錄

```bash
./flash.sh f103          # = build + probe-rs download + reset（序號自動偵測）
# 等同：
cd stm32f103-target && cargo build --release
probe-rs download --chip STM32F103C8Tx --probe 2e8a:000c-0:<serial> --protocol swd --speed 1000 \
  target/thumbv7m-none-eabi/release/stm32f103-target
probe-rs reset --chip STM32F103C8Tx --probe 2e8a:000c-0:<serial> --protocol swd
```

### 與 F401 的差異（移植時的重點）

1. **架構**:Cortex-M3 無 FPU → `thumbv7m-none-eabi`(非 `thumbv7em-none-eabihf`)。flash.sh 的
   `flash_stm32` 第 3 參數傳 target triple。
2. **I2C v1**:F103 的 `i2c::Config` **沒有** `scl_pullup/sda_pullup`(那是 F4 的 I2C v2)→ 移除,
   靠 OLED 模組自帶上拉。PB8/PB9 在 F1 需 I2C1 **重映射**,embassy-stm32 依所用腳位自動設定 AFIO。
3. **RDP 解保護**用 `stm32f1x unlock 0`(F1 系列;F4 是 `stm32f2x unlock 0`)。Blue Pill 多半 RDP 關著,
   通常可直接 `probe-rs download`;若連線失敗才需解。
4. **Blue Pill 山寨晶片**(CKS/CS32 等)常見:多數仍以 `STM32F103C8Tx` 正常燒;少數 flash 實為 128KB
   或 DEV_ID 略異。若 download 報容量不符,改用 `STM32F103CBTx`。
