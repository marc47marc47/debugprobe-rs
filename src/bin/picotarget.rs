//! Pico (RP2040) layer-2 測試韌體：板載 LED 閃 + OLED 狀態 + UART 雙向（RX echo + 每秒 TX 心跳）。
//! **功能對等於 `stm32f401-target`**（= `uartmon` 的 OLED+LED+RX echo ＋ `uarthello` 的週期 TX 心跳），
//! 把一顆 Pico 當 layer-2 目標，驗證 debugprobe-rs 探針：SWD 燒錄/重置 + UART 雙向橋接 + 共地。
//!
//! 接線（目標 Pico B；自身 USB 供電；與探針共地）：
//! - SWD：板底 3 針 debug 接頭 ← 探針 A.GP2(SWCLK)/A.GP3(SWDIO)/GND
//! - UART：B.GP0(UART0 TX)→探針 A.GP5(UART1 RX)、B.GP1(UART0 RX)←探針 A.GP4(UART1 TX)、GND
//! - OLED（SSD1306 I2C1）：SCL=GP7(pin10)、SDA=GP6(pin9)、VCC=3V3、GND
//! - LED：GP25（板載，免接線）
//!
//! 建置：cargo build --release --no-default-features --features rp2040 --bin picotarget
//! 燒錄：probe-rs download --chip RP2040 --probe 2e8a:000c --protocol swd --speed 1000 \
//!         target/thumbv6m-none-eabi/release/picotarget
//!       probe-rs reset --chip RP2040 --probe 2e8a:000c --protocol swd

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::UART0;
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_time::{Duration, Timer};
use embedded_io_async::{Read, Write};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

#[path = "../testkit.rs"]
mod testkit;

// UART0 採 BufferedUart（中斷驅動）以便 async 收發（才能 select RX vs 心跳逾時）。
bind_interrupts!(struct Irqs {
    UART0_IRQ => BufferedInterruptHandler<UART0>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // OLED（I2C1: SCL=GP7, SDA=GP6）
    let mut oled = testkit::Oled3::new(p.I2C1, p.PIN_7, p.PIN_6);
    oled.draw("pico layer2", "UART0 GP0/GP1", "waiting...");

    // UART0（TX=GP0, RX=GP1），115200，BufferedUart 以便 async。
    let mut cfg = UartConfig::default();
    cfg.baudrate = 115_200;
    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let mut uart = BufferedUart::new(
        p.UART0,
        p.PIN_0,
        p.PIN_1,
        Irqs,
        TX_BUF.init([0; 256]),
        RX_BUF.init([0; 256]),
        cfg,
    );

    // 板載 LED（GP25）：每次事件翻轉。
    let mut led = Output::new(p.PIN_25, Level::Low);

    let mut tx_n: u32 = 0;
    let mut rx_total: u32 = 0;
    let mut buf = [0u8; 64];
    let mut last_line: heapless::String<21> = heapless::String::new();
    loop {
        // select：先到者勝。RX 有資料 → echo + 顯示；逾 1s 無資料 → 送 TX 心跳。
        match select(uart.read(&mut buf), Timer::after(Duration::from_millis(1000))).await {
            Either::First(Ok(len)) if len > 0 => {
                let _ = uart.write_all(&buf[..len]).await; // echo 回探針 → host（驗反向橋接）
                rx_total = rx_total.wrapping_add(len as u32);
                led.toggle();
                for &c in &buf[..len] {
                    if c == b'\r' || c == b'\n' || last_line.len() >= 21 {
                        last_line.clear();
                    } else {
                        let _ = last_line.push(c as char);
                    }
                }
                let mut l3: heapless::String<21> = heapless::String::new();
                let _ = write!(l3, "tx{} rx{}", tx_n, rx_total);
                oled.draw("pico layer2", last_line.as_str(), l3.as_str());
            }
            Either::Second(()) => {
                let mut line: heapless::String<48> = heapless::String::new();
                let _ = write!(line, "hello from pico #{}\r\n", tx_n);
                let _ = uart.write_all(line.as_bytes()).await;
                tx_n = tx_n.wrapping_add(1);
                led.toggle();
                let mut l3: heapless::String<21> = heapless::String::new();
                let _ = write!(l3, "tx{} rx{}", tx_n, rx_total);
                oled.draw("pico layer2", last_line.as_str(), l3.as_str());
            }
            _ => {}
        }
    }
}
