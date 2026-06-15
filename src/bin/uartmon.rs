//! 測試用目標韌體：在目標 B 上監看 UART0 RX，顯示到 OLED，並 echo 回傳。
//! 用來驗證 USB→UART 反向橋接（host 寫 COM → 探針 → 目標 B.RX）+ 目標 OLED。
//!
//! 接線：UART0 TX=GP0(pin1)、RX=GP1(pin2)；OLED I2C1 SCL=GP7(pin10)、SDA=GP6(pin9)。
//! 建置：cargo build --release --no-default-features --features rp2040 --bin uartmon

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use embassy_rp::i2c::{Config as I2cConfig, I2c};
use embassy_rp::uart::{Config as UartConfig, Uart};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // OLED（I2C1: SCL=GP7, SDA=GP6）
    let i2c = I2c::new_blocking(p.I2C1, p.PIN_7, p.PIN_6, I2cConfig::default());
    let iface = I2CDisplayInterface::new(i2c);
    let mut oled = Ssd1306::new(iface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    let _ = oled.init();
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

    let mut draw = |l1: &str, l2: &str, l3: &str| {
        let _ = oled.clear(BinaryColor::Off);
        let _ = Text::with_baseline(l1, Point::new(0, 0), style, Baseline::Top).draw(&mut oled);
        let _ = Text::with_baseline(l2, Point::new(0, 16), style, Baseline::Top).draw(&mut oled);
        let _ = Text::with_baseline(l3, Point::new(0, 32), style, Baseline::Top).draw(&mut oled);
        let _ = oled.flush();
    };
    draw("uartmon (target)", "UART0 GP0/GP1", "waiting RX...");

    // UART0（TX=GP0, RX=GP1）
    let mut cfg = UartConfig::default();
    cfg.baudrate = 115_200;
    let mut uart = Uart::new_blocking(p.UART0, p.PIN_0, p.PIN_1, cfg);

    let mut line: heapless::String<21> = heapless::String::new();
    let mut total: u32 = 0;
    loop {
        let mut b = [0u8; 1];
        if uart.blocking_read(&mut b).is_ok() {
            let _ = uart.blocking_write(&b); // echo 回 host
            total = total.wrapping_add(1);
            let c = b[0];
            if c == b'\r' || c == b'\n' || line.len() >= 21 {
                line.clear();
            } else {
                let _ = line.push(c as char);
            }
            let mut cnt: heapless::String<21> = heapless::String::new();
            let _ = write!(cnt, "rx bytes: {}", total);
            draw("uartmon (target)", line.as_str(), cnt.as_str());
        }
    }
}
