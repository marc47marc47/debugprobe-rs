# 接線計畫：Nucleo STM32F446RE 作為 layer 2 目標

**目標**：用 debugprobe-rs 探針（通用 CMSIS-DAP SWD）把 **Nucleo-F446RE（STM32F446RET6,
Cortex-M4F, 512KB flash / 128KB SRAM）** 當 layer 2——SWD 燒錄/重置 + USB-to-UART 橋接 +
外接 OLED 狀態 + 板載 LED 閃爍。本檔聚焦**接線、跳線插法、OLED 接法**。

> 向量圖版本見 [`TEST-stm32f446re.html`](TEST-stm32f446re.html)（瀏覽器開，內嵌 SVG）。
> 韌體 crate：`stm32f446-target/`（鏡像 `stm32f401-target/`，LED 改 PA5、feature `stm32f446re`）。

> **與 Black Pill 最大不同**：Nucleo **有板載 ST-LINK/V2-1**。要讓**外部探針**接管 onboard
> STM32F446 的 SWD，**必須先移除 CN2 兩顆跳線**，否則板載 ST-LINK 會與探針在 SWD 上衝突。

---

## 1. 跳線插法（最重要，先做）

| 跳線 | 預設 | 本測試設定 | 原因 |
|---|---|---|---|
| **CN2（兩顆，標 "ST-LINK"）** | 插上 | **★ 移除兩顆 ★** | 斷開板載 ST-LINK 與 onboard 目標的 SWDIO/SWCLK，讓外部探針獨佔 SWD |
| **JP6（IDD）** | 插上 | **保持插上** | 目標 MCU 供電路徑；拔掉則目標沒電（DP 都讀不到） |
| 電源選擇 JP（U5V/E5V，依板 rev 為 JP5/JP1） | U5V | **保持 U5V（預設）** | 由 Nucleo 自身 CN1（ST-LINK USB）供電；移除 CN2 不影響供電 |

- **供電**：Nucleo 插自身 **CN1 USB（ST-LINK 那個 USB 孔）**；板上紅色 **LD3 電源燈會亮**。
- 與探針**只共地、不反灌電**（不要從探針灌 3V3 進 Nucleo）。

```
         Nucleo-F446RE（俯視，ST-LINK 在上半部）
   ┌──────────────────────────────────────────┐
   │  CN1 USB(ST-LINK)         CN2  [▯] [▯]      │  ← CN2 兩顆跳線：拔掉
   │                                ↑ 拔掉後      │
   │                          目標側針=SWDIO/SWCLK│
   │   ST-LINK MCU            JP6(IDD)=插著        │
   │  ───────────（切割線）───────────            │
   │   STM32F446RET6  目標 MCU                     │
   │   morpho CN7(左)            morpho CN10(右)   │
   │   Arduino CN6/CN8(左)       CN5/CN9(右)       │
   └──────────────────────────────────────────┘
```

---

## 2. 接線（探針 A〔board-pico〕→ Nucleo-F446RE）

| 探針 A | 訊號 | F446 目標腳 | 板上位置（建議用 Arduino 絲印，最可靠） |
|---|---|---|---|
| GP2 | SWCLK | PA14 | CN7 morpho 的 PA14，或**移除 CN2 後的目標側針** |
| GP3 | SWDIO | PA13 | CN7 morpho 的 PA13，或 CN2 目標側針 |
| GP1 | nRESET | NRST | **CN6**（Arduino 電源排，絲印 `NRST`） |
| GP4 | UART1 TX → | PA10（USART1 RX） | Arduino **D2**（CN9） |
| GP5 | ← UART1 RX | PA9（USART1 TX） | Arduino **D8**（CN5） |
| GND | 共地 | GND | **CN6** GND（就近、獨立短線，緊貼 SWDIO） |

- **避開 PA2 / PA3（USART2）**：經 SB13/SB14 接到 ST-LINK 的虛擬序列埠（VCP），會爭線。
  → 改用 **USART1（PA9/PA10）**，與 STM32F401 版韌體相同。
- **SWD 用短線、就近接地**（沿用 F401 教訓：長杜邦線會「DP 讀得到、AHB 時好時壞」，**降頻無效**）。

```
        主機 PC
          │ USB（CMSIS-DAP v2 + CDC-ACM）
 ┌────────┴───────────┐                          ┌────────────────────────────┐
 │  Probe A (Pico)    │           SWD            │  Nucleo-F446RE（CN2 已移除）│
 │  debugprobe-rs     │                          │  （CN1 USB 自身供電）        │
 │                    │                          │                             │
 │  GP2  SWCLK  ──────┼─────────────────────────▶│  PA14 SWCLK (CN7 / CN2 目標側)│
 │  GP3  SWDIO  ◀─────┼─────────────────────────▶│  PA13 SWDIO                 │
 │  GP1  nRESET ──────┼─────────────────────────▶│  NRST (CN6)                 │
 │  GP4  UART1 TX ────┼─────────────────────────▶│  PA10 USART1 RX (D2)        │
 │  GP5  UART1 RX ◀───┼──────────────────────────│  PA9  USART1 TX (D8)        │
 │  GND  ─────────────┼──────────────────────────│  GND (CN6)                  │
 └────────────────────┘                          │                             │
 ┌────────────────────┐           I2C            │                             │
 │  SSD1306 OLED 0.96  │                          │                             │
 │  SCL ◀─────────────┼──────────────────────────│  PB8 I2C1 SCL (D15)         │
 │  SDA ◀────────────▶┼──────────────────────────│  PB9 I2C1 SDA (D14)         │
 │  VCC / GND         │                          │  3V3 / GND (CN6)            │
 └────────────────────┘                          │  LED：PA5 (LD2, 板載)       │
                                                 └────────────────────────────┘
```

> **SWD 取點注意**：PA13/PA14 不在 Arduino 排上。最穩做法是接 **CN2 移除跳線後的目標側針**
> （朝目標 MCU 那一側，非朝 ST-LINK 那側）；或 CN7 morpho 的 PA13/PA14（針號以板上絲印 / UM1724 為準）。

---

## 3. OLED 接法（SSD1306 0.96" 128×64, I2C1）

| OLED 腳 | 接到 F446 | Arduino 絲印 |
|---|---|---|
| SCL | PB8（I2C1 SCL） | **D15**（CN5） |
| SDA | PB9（I2C1 SDA） | **D14**（CN5） |
| VCC | 3V3 | **CN6** 3V3 |
| GND | GND | CN6 GND |

- 多數 SSD1306 模組自帶上拉電阻;韌體也已開內部上拉（保險）。
- I2C 位址預設 0x3C(韌體用 ssd1306 預設)。
- 板載 **LED = PA5（LD2，active-high）**,韌體會閃爍,**免接線**。

---

## 4. 燒錄與驗證（接好線、移除 CN2 後）

```bash
cd stm32f446-target && cargo build --release

# 連線（Nucleo 通常無 RDP、demo 不 sleep；若 AHB 不通再加 --connect-under-reset）
probe-rs info     --chip STM32F446RETx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000

# 燒錄 + 重置
probe-rs download --chip STM32F446RETx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd --speed 1000 \
  target/thumbv7em-none-eabihf/release/stm32f446-target
probe-rs reset    --chip STM32F446RETx --probe 2e8a:000c-0:E6605838834DA330 --protocol swd
```

**驗證點**：
1. `probe-rs info` 讀到 F446 的 DP/AP/Cortex-M4 core。
2. 燒錄成功、reset 後板載 **LD2(PA5) 閃爍**。
3. OLED 顯示 `f446 layer2` / 最後 RX 文字 / `tx{n} rx{n}` 計數。
4. 主機開探針 CDC COM @115200：收到 `hello from f446 #n`（TX 方向）；打字 → F446 echo 回（RX 方向）。

---

## 4b. 實測結果（2026-06-16）✅

| 驗證項 | 結果 |
|---|---|
| SWD 連線（DPIDR 0x2BA01477 / device id 0x421 / ROM table） | ✅ |
| 清除 RDP Level 1（`stm32f2x unlock`，mass erase） | ✅ |
| **SWD 燒錄**（32768 B，vector `SP=0x20020000` / `PC=0x080001c5`） | ✅ |
| 韌體運行（計數遞增） | ✅ |
| UART TX `hello from f446 #n` | ✅ 乾淨 |
| UART RX echo | ✅ |

> **本顆 F446 出廠帶 RDP Level 1**（Nucleo 少見但確有），且 **NRST 與板載 ST-LINK 共線**，
> 導致**任何走 reset 的燒錄**（`probe-rs download`、OpenOCD `program`）都卡在「halt 後 reset timeout」。
> 但**純 `halt` 其實成功**（DHCSR=`0x00030003`，S_HALT=1）。最終可行流程（避開 reset-halt）：

```bash
# 1. 清 RDP（connect_assert_srst；印 "unlocked." 後拔插 CN1 USB 重新上電）
openocd -f interface/cmsis-dap.cfg -c "transport select swd" -c "adapter speed 1000" \
  -f target/stm32f4x.cfg -c "reset_config srst_only srst_nogate connect_assert_srst" \
  -c "init" -c "catch { reset halt }" -c "catch { halt }" -c "stm32f2x unlock 0" -c "reset run" -c "shutdown"

# 2. 燒錄：halt → flash write_image（不走會卡的 reset-halt）→ SYSRESETREQ run
openocd -f interface/cmsis-dap.cfg -c "transport select swd" -c "adapter speed 1000" \
  -f target/stm32f4x.cfg -c "reset_config none" -c "cortex_m reset_config sysresetreq" \
  -c "init" -c "catch { halt }" \
  -c "flash write_image erase stm32f446-target/target/thumbv7em-none-eabihf/release/stm32f446-target" \
  -c "catch { reset run }" -c "shutdown"
```

> `probe-rs download` 在本板因共線 NRST 的 reset 序列 timeout 而失敗；OpenOCD 的 `halt + flash write_image`
> 是繞過此問題的可靠路徑。若你的 Nucleo NRST 沒有共線干擾，`probe-rs download` 應可直接用。

## 5. 疑難排解（沿用 STM32 經驗，見 `MULTI-TARGET.md`）

| 現象 | 處置 |
|---|---|
| DP 讀得到、AHB/記憶體存取失敗 | 多半是 **CN2 沒移除**(ST-LINK 爭線) 或供電不足;確認 CN2 拔掉、CN1 USB 供電(LD3 亮) |
| `Protocol error` / 連線時好時壞 | SWD 杜邦線太長 → 換短線、GND 就近;探針已內建 2mA 弱驅動+慢 slew(降頻無效) |
| 初次連線進不去 | 加 `--connect-under-reset`(已接 GP1→NRST) |
| UART 全亂碼 | 多半 baud 不符或 TX/RX 接反;確認 A.GP4→PA10、A.GP5←PA9、共地、115200 |
| UART 浮接雜訊（FF/FE） | RX 線沒接到目標;檢查 A.GP5 ↔ PA9 |

> 註:Nucleo 多半無 RDP,但**本實測這顆 F446 帶 RDP Level 1**,流程與 F401 相同(先 `stm32f2x unlock`)。
> 另本板 NRST 與 ST-LINK 共線 → 用 `halt + flash write_image`(見 §4b),不要走 reset-based 的 download。
