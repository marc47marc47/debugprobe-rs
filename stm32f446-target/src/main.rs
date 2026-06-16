//! Nucleo-F446RE layer-2 測試韌體：板載 LED 閃爍 + 外接 OLED 狀態 + UART 雙向 rx/tx。
//! 對等於 RP2040 的 `src/bin/uartmon.rs`（OLED+LED+RX echo）＋ `uarthello.rs`（週期 TX 心跳），
//! 用來驗證 debugprobe-rs 探針可把 STM32F446 當作 layer 2 目標（SWD 燒錄/重置 + UART 橋接）。
//!
//! 接線（Nucleo 有板載 ST-LINK：**先移除 CN2 兩顆跳線**，自身 CN1 USB 供電，探針共地）：
//! - SWD：探針 A.GP2→SWCLK(PA14)、A.GP3→SWDIO(PA13)、A.GP1→NRST、GND（PA13/PA14 在 CN7 morpho 或 CN2 目標側）
//! - UART：A.GP4(UART1 TX)→PA10(USART1 RX, Arduino D2)、A.GP5(UART1 RX)←PA9(USART1 TX, Arduino D8)、GND
//! - OLED（SSD1306 I2C1）：SCL=PB8(D15)、SDA=PB9(D14)、VCC=3V3、GND
//! - LED：PA5（LD2，板載，active-high，免接線）
//!
//! 建置：cd stm32f446-target && cargo build --release
//! 燒錄：probe-rs download --chip STM32F446RETx --probe 2e8a:000c-0:<serial> --protocol swd --speed 1000 \
//!         target/thumbv7em-none-eabihf/release/stm32f446-target

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::bind_interrupts;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
use embassy_stm32::peripherals::USART1;
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_time::{Duration, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use embedded_io_async::{Read, Write};
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// USART1 採 BufferedUart（中斷驅動），故需綁定其中斷。I2C 用 blocking，不需綁。
bind_interrupts!(struct Irqs {
    USART1 => BufferedInterruptHandler<USART1>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    // OLED（I2C1：SCL=PB8, SDA=PB9）；400kHz，並開內部上拉以防麵包板缺上拉電阻。
    let mut i2c_cfg = I2cConfig::default();
    i2c_cfg.frequency = Hertz::khz(400);
    i2c_cfg.scl_pullup = true;
    i2c_cfg.sda_pullup = true;
    let i2c = I2c::new_blocking(p.I2C1, p.PB8, p.PB9, i2c_cfg);
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
    draw("f446 layer2", "USART1 PA9/PA10", "waiting...");

    // USART1（RX=PA10, TX=PA9），115200，BufferedUart 以便 async 收發。
    let mut uart_cfg = UartConfig::default();
    uart_cfg.baudrate = 115_200;
    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let mut uart = BufferedUart::new(
        p.USART1,
        p.PA10,
        p.PA9,
        TX_BUF.init([0; 256]),
        RX_BUF.init([0; 256]),
        Irqs,
        uart_cfg,
    )
    .unwrap();

    // 板載 LED（PA5 / LD2，Nucleo，active-high）。
    let mut led = Output::new(p.PA5, Level::Low, Speed::Low);

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
                draw("f446 layer2", last_line.as_str(), l3.as_str());
            }
            Either::Second(()) => {
                let mut line: heapless::String<48> = heapless::String::new();
                let _ = write!(line, "hello from f446 #{}\r\n", tx_n);
                let _ = uart.write_all(line.as_bytes()).await;
                tx_n = tx_n.wrapping_add(1);
                led.toggle();
                let mut l3: heapless::String<21> = heapless::String::new();
                let _ = write!(l3, "tx{} rx{}", tx_n, rx_total);
                draw("f446 layer2", last_line.as_str(), l3.as_str());
            }
            _ => {}
        }
    }
}
