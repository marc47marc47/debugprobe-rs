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
}
