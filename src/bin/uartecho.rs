//! 壓測用目標韌體：UART0 全速 echo（RX→TX），量 UART 橋接最高 baud/吞吐用。
//! 刻意**不加 OLED**（blocking I2C 會拖累 echo、汙染吞吐量測）；僅 GP25 LED 心跳。
//! baud 為編譯期 const；sweep 時改 `BAUD` 重編譯、用 A 經 probe-rs 燒進 B。
//!
//! 接線：UART0 TX=GP0(pin1)、RX=GP1(pin2)。
//! 建置：cargo build --release --no-default-features --features rp2040 --bin uartecho

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config};
use embassy_rp::{bind_interrupts, peripherals::UART0};
use embedded_io_async::{Read, Write};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

/// 壓測 baud（sweep 時改此值重編譯）。
const BAUD: u32 = 921_600;

bind_interrupts!(struct Irqs {
    UART0_IRQ => BufferedInterruptHandler<UART0>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut cfg = Config::default();
    cfg.baudrate = BAUD;
    static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    let mut uart = BufferedUart::new(
        p.UART0,
        p.PIN_0,
        p.PIN_1,
        Irqs,
        TX_BUF.init([0; 1024]),
        RX_BUF.init([0; 1024]),
        cfg,
    );

    let mut led = Output::new(p.PIN_25, Level::Low);

    let mut buf = [0u8; 256];
    let mut since_blink: u32 = 0;
    loop {
        match uart.read(&mut buf).await {
            Ok(n) if n > 0 => {
                let _ = uart.write_all(&buf[..n]).await;
                since_blink += n as u32;
                if since_blink >= 4096 {
                    since_blink = 0;
                    led.toggle();
                }
            }
            _ => {}
        }
    }
}
