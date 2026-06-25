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
use embassy_rp::uart::{Config as UartConfig, UartTx};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

#[path = "../testkit.rs"]
mod testkit;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // OLED（I2C1: SCL=GP7, SDA=GP6）
    let mut oled = testkit::Oled3::new(p.I2C1, p.PIN_7, p.PIN_6);

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
        oled.draw("uarthello (target)", "UART0 TX 115200", s.as_str());

        led.toggle();
        n = n.wrapping_add(1);
        Timer::after(Duration::from_millis(500)).await;
    }
}
