# 接線計畫：Anycubic Vyper / TriGorilla+ v0.0.6 作為 layer 2 目標

**目標**：用 debugprobe-rs 探針(通用 CMSIS-DAP SWD)對 Anycubic Vyper 主板 **TriGorilla+ v0.0.6**
做 layer 2 SWD 連線/燒錄測試。

> ⚠️ **本檔為「接線計畫 + 焊點定位指南」,尚未實機操作**(依使用者決定:先只出文件,不碰硬體)。
> **這是一台實體 3D 印表機主板,不是開發板** —— 動手前務必讀完 §5 安全須知。

---

## 0. 板子事實(已查證)

| 項目 | 值 | 來源 |
|---|---|---|
| MCU | **GigaDevice GD32F103RET6**(STM32F103 相容,**Cortex-M3**,非 M4) | pasha4ur 文章 / anycubic-vyper-doc |
| Flash / RAM | 512 KB / 64 KB | 同上 |
| 時脈 | 108 MHz | 同上 |
| 平常更新方式 | **SD 卡 bootloader**(讀 `main_board_xxxxxxxx.bin`) | Klipper Vyper 指南 |
| 暴露的 UART | USART0 PA9/PA10(→板載 CH340 USB-serial)、USART1 PA2/PA3(WiFi)、USART2 PB10/PB11(LCD) | pasha4ur 文章 |
| SWD 焊點位置 | **公開資料皆未記載**(見 §2 定位方法) | Anycubic issue #4 未公開 schematic |

- **target triple**:Cortex-M3 → `thumbv7m-none-eabi`(無 FPU,與前面 M4F 的 `thumbv7em-none-eabihf` 不同)。
- **probe-rs chip 名**:用相容名 **`STM32F103RETx`** 最穩(GD32F103 的 flash 程式化與 STM32F103 相容);
  probe-rs 的 GD32F10x 系列亦可試。
- **embassy-stm32 不支援 GD32**(GD32F103 與 STM32F103 暫存器相容,`stm32f103` feature 常能跑但屬**非官方**)
  —— 這影響「測試韌體」階段,本接線文件不受影響。

---

## 1. SWD 接線(探針 A → TriGorilla+)

GD32F103 的 SWD 是**固定腳位**:SWDIO=**PA13**、SWCLK=**PA14**。NRST=晶片 RESET 腳。

| 探針 A | 訊號 | GD32F103 | 備註 |
|---|---|---|---|
| GP2 | SWCLK | PA14 | |
| GP3 | SWDIO | PA13 | |
| GP1 | nRESET | NRST | 接了較穩(GD32 偶需 connect-under-reset) |
| GND | 共地 | GND | **必接**,就近短線 |
| ~~3V3~~ | ~~電源~~ | ~~3V3~~ | **★ 不要接 ★** 印表機自己供電,反灌電會燒板 |

> **供電**:用印表機**自身的 USB 線或 24V 電源**供電,探針與板子**只共地**。
> (社群 Predator/TriGorilla 指南原文:*"don't need VCC 5v or 3.3v (please don't connect it you could damage your board)"*)

---

## 2. 如何定位 SWD 焊點(公開資料未記載,需自己找)

PA13/PA14 是 GD32F103RET6(**LQFP64** 封裝)的固定接腳。LQFP64 上:
- **PA13 ≈ pin 46**、**PA14 ≈ pin 49**、PA15 ≈ pin 50、**NRST ≈ pin 7**
  （以 GD32F103RET6 datasheet 腳位圖為準;晶片上有 pin1 圓點/缺角標記）。

定位步驟(三選一,由易到難):
1. **找測試焊點群**:GD32 旁常有一排 4~5 個小焊點/via(SWDIO/SWCLK/GND/3V3/RST),
   或在 SD 卡座、CH340 附近。先目視找「一排沒接元件的金屬點」。
2. **萬用表連通量測**(最可靠):晶片斷電下,用萬用表 continuity,從 GD32 的 **PA13(pin46)**、
   **PA14(pin49)** 腳,逐一點測附近焊點,**嗶聲**那點就是對應 SWD 訊號。GND 找任一接地大銅面。
3. **直接焊飛線到晶片腳**(最後手段,需細焊功夫):直接從 PA13/PA14/NRST 腳引飛線。

> 找到後**拍照標記**,之後實機階段直接用。建議先量 3V3 焊點確認電壓(~3.3V)、確認 GND。

---

## 3. UART(供日後 bridge 測試,本階段僅規劃)

印表機板**多數 GPIO 接步進/加熱/風扇**,不可亂用。可安全用於 UART 橋接測試的是**暴露的 UART 排針**:

| 用途 | GD32 腳 | 接探針 | 備註 |
|---|---|---|---|
| 建議:USART1（WiFi 排針) | PA2(TX)/PA3(RX) | A.GP5←PA2、A.GP4→PA3 | WiFi 排針通常可接,較安全 |
| 備選:USART0 | PA9(TX)/PA10(RX) | — | **接到板載 CH340 → 走印表機自己的 USB**,不需探針即可看 |
| 勿用:USART2 | PB10/PB11 | — | 接 LCD,佔用會讓螢幕失效 |

---

## 4. OLED(可選,本階段僅規劃)

需要一組**確認空閒**的 I2C 腳(GD32F103 I2C1=PB6/PB7 或 PB8/PB9;I2C2=PB10/PB11)。
**但印表機板這些腳極可能已接其他功能**(PB10/PB11 是 LCD)→ **務必先確認該腳在 TriGorilla+ 上未被佔用**再接,
否則略過 OLED、改用 UART(§3)觀察即可。SSD1306:SCL/SDA/VCC(3V3)/GND。

---

## 5. ⚠️ 安全須知(實機印表機,務必遵守)

1. **加熱/馬達風險**:測試韌體若誤觸加熱床/噴頭/馬達腳,可能過熱起火或撞機。
   → 燒測試韌體時,**關閉 24V 電源、只用 USB 供電**(或拔掉加熱器/馬達接頭),只跑安全腳位。
2. **覆蓋原廠韌體 + bootloader**:SWD 燒錄會蓋掉 Anycubic 原廠韌體;若 **RDP 讀保護開啟**,
   清除 RDP 會 **mass erase**,**連 SD 卡 bootloader 一起清掉** → 之後 **SD 卡更新失效**,
   只能再用 SWD 把**完整韌體(含 bootloader)**刷回去才能恢復列印。
3. **先備份**:實機第一步應是**非破壞性連線 + 嘗試讀回 flash 備份**(若 RDP=0 可讀;RDP=1 則讀不出,
   清除即永久失去原廠韌體)。**動手前請先取得可恢復的完整 Vyper 韌體**。
4. **不要接 3V3/5V**(§1);只共地,板子自身供電。

---

## 6. 後續流程(實機階段,待另行授權)

1. 非破壞性:`probe-rs info --chip STM32F103RETx --probe 2e8a:000c-0:E660... --protocol swd --speed 1000`
   → 確認認得 GD32 core、檢查 RDP 狀態。
2. 若 RDP=0:`probe-rs read`/openocd `dump_image` 備份原廠 flash。
3. 若要燒:RDP=1 需先 unlock(同 STM32:openocd `stm32f1x unlock 0`,**會 mass erase**),
   再燒測試韌體(GD32 用 `stm32f103` 相容路徑或最小 bare-metal blink)。
4. 驗證:LED/安全腳閃爍 + UART(§3)雙向。

> 測試韌體(GD32 因非 STM32,embassy 屬非官方)的選型待實機階段再決定:
> 最小 bare-metal blink(最穩,純驗證 SWD 燒錄)、或試 embassy-stm32 `stm32f103`(功能完整但非官方)。

---

## 來源

- [pasha4ur Vyper 指南(MCU/UART 腳位)](https://pasha4ur.org.ua/articles/anycubic-vyper-overviews-guides-adjustments-settings-tips-modifications-upgrades-and-custom-firmwares/4)
- [anycubic-vyper-doc:TriGorilla V0.0.6 Mainboard](https://anycubic-vyper-doc.readthedocs.io/en/latest/hardware/components/trigorilla_v006_mainboard.html)
- [Klipper Vyper 安裝指南(SD 卡刷法)](https://sean-dearing.gitbook.io/klipper-installation-for-anycubic-vyper/klipper-firmware/klipper-firmware-flashing)
- [Marlin Predator TriGorilla Pro(ST-Link/SWD;"不要接 VCC" 警語)](https://github.com/SXHXC/Marlin-Anycubic-Predator-Trigorilla-PRO)
- [Anycubic 官方 Vyper issue #4(schematic 未公開)](https://github.com/ANYCUBIC-3D/Vyper/issues/4)
