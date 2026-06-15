//! 測試用目標韌體：在 UART0 TX (GP0) 以 115200 持續印字。
//! 燒進目標 B，經原廠接線（B.GP0 → A.GP5/UART1 RX）由探針橋接到主機 COM 埠，
//! 用來端對端驗證「SWD 燒錄 + UART 橋接」。
//!
//! 建置：cargo build --release --no-default-features --features rp2040 --bin uarthello

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use embassy_rp::uart::{Config, UartTx};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut cfg = Config::default();
    cfg.baudrate = 115_200;
    let mut tx = UartTx::new_blocking(p.UART0, p.PIN_0, cfg);

    let mut n: u32 = 0;
    loop {
        let mut line: heapless::String<48> = heapless::String::new();
        let _ = write!(line, "hello from target #{}\r\n", n);
        let _ = tx.blocking_write(line.as_bytes());
        n = n.wrapping_add(1);
        Timer::after(Duration::from_millis(500)).await;
    }
}
