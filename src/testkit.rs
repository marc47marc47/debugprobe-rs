//! 測試 bin（src/bin/*）共用的小工具：避免每個 bin 重抄 OLED 初始化 + 三行繪製。
//!
//! 不屬於探針主韌體模組樹（main.rs 不 `mod testkit`），各測試 bin 以
//! `#[path = "../testkit.rs"] mod testkit;` 引入。故以 `--features rp2040`（無 board-*）
//! 即可編譯。

use embassy_rp::Peri;
use embassy_rp::i2c::{Blocking, Config as I2cConfig, I2c};
use embassy_rp::peripherals::{I2C1, PIN_6, PIN_7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use ssd1306::mode::BufferedGraphicsMode;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

type Oled =
    Ssd1306<I2CInterface<I2c<'static, I2C1, Blocking>>, DisplaySize128x64, BufferedGraphicsMode<DisplaySize128x64>>;

/// 測試目標板的 OLED（SSD1306 128x64，I2C1 SCL=GP7/SDA=GP6），三行文字顯示。
pub struct Oled3 {
    oled: Oled,
    style: MonoTextStyle<'static, BinaryColor>,
}

impl Oled3 {
    /// 初始化 OLED（SCL=GP7, SDA=GP6，預設 I2C 速率）。
    pub fn new(i2c: Peri<'static, I2C1>, scl: Peri<'static, PIN_7>, sda: Peri<'static, PIN_6>) -> Self {
        let i2c = I2c::new_blocking(i2c, scl, sda, I2cConfig::default());
        let iface = I2CDisplayInterface::new(i2c);
        let mut oled = Ssd1306::new(iface, DisplaySize128x64, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();
        let _ = oled.init();
        Self {
            oled,
            style: MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        }
    }

    /// 清屏並繪製三行文字（y = 0 / 16 / 32），刷新到面板。
    pub fn draw(&mut self, l1: &str, l2: &str, l3: &str) {
        let _ = self.oled.clear(BinaryColor::Off);
        for (i, line) in [l1, l2, l3].iter().enumerate() {
            let y = (i as i32) * 16;
            let _ = Text::with_baseline(line, Point::new(0, y), self.style, Baseline::Top)
                .draw(&mut self.oled);
        }
        let _ = self.oled.flush();
    }
}
