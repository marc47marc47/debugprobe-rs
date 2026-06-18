//! 板上 OLED 狀態顯示（SSD1306 128x64 I2C，BufferedGraphicsMode）。
//!
//! 用 embedded-graphics 以像素座標繪製多行文字（TerminalMode 在此面板只顯示
//! 末字元，故改用整張 framebuffer 刷新）。OLED 為選用：未接/init 失敗則靜默略過。
//!
//! 接線（Pico）：GND→GND, VCC→3V3(pin36), SCL→GP7(pin10), SDA→GP6(pin9)。

use embassy_rp::i2c::{Blocking, I2c};
use embassy_rp::peripherals::I2C1;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use ssd1306::mode::BufferedGraphicsMode;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

type Iface = I2CInterface<I2c<'static, I2C1, Blocking>>;
type Oled = Ssd1306<Iface, DisplaySize128x64, BufferedGraphicsMode<DisplaySize128x64>>;

/// 右側柱狀圖一條：label（≤2 字）+ 數值/上限（畫成長度 = value/max 的橫條）。
pub struct Bar {
    pub label: &'static str,
    pub value: u32,
    pub max: u32,
}

/// OLED 一幀的顯示模型（由 oled_task 組裝、`DebugOled::render` 繪製）。
pub struct OledModel<'a> {
    /// 第 1 行：晶片型號 / "no target"。
    pub chip: &'a str,
    /// 第 2 行：可燒錄狀態（RDP）+ 頻率。
    pub flash: &'a str,
    /// SWCLK/SWDIO token-ring 環形緩衝（128 欄各 1 bit，4×u32）。
    pub clk: [u32; 4],
    pub dio: [u32; 4],
    /// ring 最舊欄（由舊→新繪製 → 畫面向左捲動）。
    pub pos: usize,
    /// 左下角文字（連線品質 DP/AP；無目標時空字串）。
    pub scale: &'a str,
    /// 右側柱狀圖（Ce/De/h… 等狀態），由上而下排列。
    pub bars: &'a [Bar],
}

pub struct DebugOled {
    oled: Option<Oled>,
}

impl DebugOled {
    /// 嘗試初始化 OLED；失敗則回傳 no-op 實例（不顯示）。
    pub fn new(i2c: I2c<'static, I2C1, Blocking>) -> Self {
        let interface = I2CDisplayInterface::new(i2c);
        let mut oled = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();
        let ok = oled.init().is_ok();
        Self {
            oled: if ok { Some(oled) } else { None },
        }
    }

    /// 清空並逐行繪製狀態文字，然後刷新到面板。
    pub fn status(&mut self, lines: &[&str]) {
        let Some(o) = &mut self.oled else { return };
        let _ = o.clear(BinaryColor::Off);
        let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
        let mut y = 0i32;
        for line in lines {
            let _ = Text::with_baseline(line, Point::new(0, y), style, Baseline::Top).draw(o);
            y += 12;
        }
        if o.flush().is_err() {
            let _ = o.init();
        }
    }

    /// SWD 數位邏輯波形顯示（2 通道方波,像邏輯示波器；token-ring 捲動）：
    /// 上方晶片型號 / 可燒狀態（y=0/12）;下方 SWCLK(C)、SWDIO(D) 兩通道方波;第 5 行刻度/訊號儀。
    pub fn render(&mut self, m: &OledModel) {
        let Some(o) = &mut self.oled else { return };
        let _ = o.clear(BinaryColor::Off);
        let text = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
        let stroke = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

        // 上方文字 2 行（晶片 / 可燒狀態）。
        for (i, line) in [m.chip, m.flash].iter().enumerate() {
            let y = (i as i32) * 12;
            let _ = Text::with_baseline(line, Point::new(0, y), text, Baseline::Top).draw(o);
        }

        // 下方兩通道方波（縮窄到左側 x=8..78，右側讓給柱狀圖）：C(SWCLK) y≈26..34、D(SWDIO) y≈42..50。
        const COLS: i32 = 70; // 波形 70 欄（x=8..78）
        // 第 c 欄 → ring 索引 (pos + c) % 128（pos=最舊,故由舊到新、向左捲動）。
        let rbit = |arr: &[u32; 4], c: i32| {
            let rc = (m.pos as i32 + c).rem_euclid(128) as usize;
            (arr[rc / 32] >> (rc % 32)) & 1 != 0
        };
        for (lbl, arr, hi) in [("C", &m.clk, 26i32), ("D", &m.dio, 42i32)] {
            let lo = hi + 8;
            let _ = Text::with_baseline(lbl, Point::new(0, hi - 1), text, Baseline::Top).draw(o);
            let lvl = |b: bool| if b { hi } else { lo };
            for c in 0..COLS {
                let x = 8 + c;
                let yc = lvl(rbit(arr, c));
                let _ = Line::new(Point::new(x, yc), Point::new(x + 1, yc))
                    .into_styled(stroke)
                    .draw(o); // 水平段
                if c > 0 && rbit(arr, c) != rbit(arr, c - 1) {
                    let yp = lvl(rbit(arr, c - 1));
                    let _ = Line::new(Point::new(x, yp), Point::new(x, yc))
                        .into_styled(stroke)
                        .draw(o); // 跳變垂直連接
                }
            }
        }
        // 左下角：連線品質文字 DP/AP（y=53）。
        let _ = Text::with_baseline(m.scale, Point::new(0, 53), text, Baseline::Top).draw(o);

        // 右側柱狀圖面板（x=82..127）：每條 label(≤2字) + 長度 = value/max 的橫條。
        const PANEL_X: i32 = 82;
        const BAR_X0: i32 = 96; // 條起點（label 後）
        const BAR_W: u32 = 30; // 條最大寬（96..126）
        let fill = PrimitiveStyle::with_fill(BinaryColor::On);
        for (i, b) in m.bars.iter().take(4).enumerate() {
            let y = 22 + i as i32 * 10;
            let _ = Text::with_baseline(b.label, Point::new(PANEL_X, y - 1), text, Baseline::Top)
                .draw(o);
            // 外框
            let _ = Rectangle::new(Point::new(BAR_X0, y), Size::new(BAR_W, 7))
                .into_styled(stroke)
                .draw(o);
            // 填充長度 = value/max
            let w = (b.value.min(b.max) * BAR_W).checked_div(b.max).unwrap_or(0);
            if w > 0 {
                let _ = Rectangle::new(Point::new(BAR_X0, y), Size::new(w, 7))
                    .into_styled(fill)
                    .draw(o);
            }
        }
        // flush 失敗（如 GND 熱拔造成 I2C 突波/SSD1306 異常）→ 重新 init，使 OLED 自癒。
        if o.flush().is_err() {
            let _ = o.init();
        }
    }
}
