//! 測試用目標韌體：在 UART0 TX (GP0) 以 115200 持續印字 + GP25 LED 心跳 + OLED 狀態。
//! 燒進目標 B，經原廠接線（B.GP0 → A.GP5/UART1 RX）由探針橋接到主機 COM 埠。
//! 也是 AutoBaud 測試的 115200 訊號源。
//!
//! 接線：UART0 TX=GP0(pin1)；OLED I2C1 SCL=GP7(pin10)、SDA=GP6(pin9)；LED=GP25。
//! 建置：cargo build --release --no-default-features --features rp2040 --bin uarthello

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{Config as I2cConfig, I2c};
use embassy_rp::uart::{Config as UartConfig, UartTx};
use embassy_time::{Duration, Timer};
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

    // UART0 TX = GP0，115200
    let mut cfg = UartConfig::default();
    cfg.baudrate = 115_200;
    let mut tx = UartTx::new_blocking(p.UART0, p.PIN_0, cfg);

    // 板載 LED (GP25) 心跳
    let mut led = Output::new(p.PIN_25, Level::Low);

    let mut n: u32 = 0;
    loop {
        let mut line: heapless::String<48> = heapless::String::new();
        let _ = write!(line, "hello from target #{}\r\n", n);
        let _ = tx.blocking_write(line.as_bytes());

        let mut s: heapless::String<21> = heapless::String::new();
        let _ = write!(s, "sent #{}", n);
        draw("uarthello (target)", "UART0 TX 115200", s.as_str());

        led.toggle();
        n = n.wrapping_add(1);
        Timer::after(Duration::from_millis(500)).await;
    }
}
