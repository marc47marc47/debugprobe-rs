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
use embedded_graphics::primitives::{Line, PrimitiveStyle};
use embedded_graphics::text::{Baseline, Text};
use ssd1306::mode::BufferedGraphicsMode;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

type Iface = I2CInterface<I2c<'static, I2C1, Blocking>>;
type Oled = Ssd1306<Iface, DisplaySize128x64, BufferedGraphicsMode<DisplaySize128x64>>;

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
        let _ = o.flush();
    }

    /// SWD 數位邏輯波形顯示（2 通道方波,像邏輯示波器；token-ring 捲動）：
    /// 上方 `lines`（晶片型號 / 可燒狀態,y=0/12）;下方 SWCLK(C)、SWDIO(D) 兩通道方波。
    /// `clk`/`dio` 為 128 欄環形緩衝（4×u32),`pos` = ring 最舊欄；由舊→新繪製→畫面向左捲動。
    pub fn status_logic(
        &mut self,
        lines: &[&str],
        clk: &[u32; 4],
        dio: &[u32; 4],
        pos: usize,
        scale: &str,
    ) {
        let Some(o) = &mut self.oled else { return };
        let _ = o.clear(BinaryColor::Off);
        let text = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
        let stroke = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

        // 上方文字 2 行（晶片 / 可燒狀態）。
        let mut y = 0i32;
        for line in lines {
            let _ = Text::with_baseline(line, Point::new(0, y), text, Baseline::Top).draw(o);
            y += 12;
        }

        // 下方兩通道方波：C(SWCLK) 在 y≈26..34、D(SWDIO) 在 y≈42..50；左標 C/D，波形 x=8..127。
        const COLS: i32 = 120; // 顯示 120 欄（x=8..127）
        // 第 c 欄 → ring 索引 (pos + c) % 128（pos=最舊,故由舊到新、向左捲動）。
        let rbit = |arr: &[u32; 4], c: i32| {
            let rc = (pos as i32 + c).rem_euclid(128) as usize;
            (arr[rc / 32] >> (rc % 32)) & 1 != 0
        };
        for (lbl, arr, hi) in [("C", clk, 26i32), ("D", dio, 42i32)] {
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
        // 第 5 行：刻度（y=53）。
        let _ = Text::with_baseline(scale, Point::new(0, 53), text, Baseline::Top).draw(o);
        let _ = o.flush();
    }
}
