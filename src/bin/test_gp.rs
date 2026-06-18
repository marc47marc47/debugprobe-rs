//! test-01-gp：探針 SWD 腳（GP2 SWCLK / GP3 SWDIO）最底層自測。
//!
//! 把 GP2/GP3 當**純 GPIO 輸出方波**（完全繞過 PIO/SWD/DAP），用來判斷 pad 本身死活：
//!   - GP2 ≈ 2 kHz 方波、GP3 ≈ 1 kHz 方波（50% duty）；GP25 板載 LED ~1s 心跳。
//!   - 用三用電表(DC) 量 GP2(pin4)/GP3(pin5) 對 GND：
//!       * ≈1.6V（3.3V 的一半）→ 有在 toggle、**pad 活著**。
//!       * 0V 或 3.3V 卡住不動 → **pad 死（latch-up 損壞）**。
//!   - 有示波器更直接：看 GP2/GP3 方波。
//!
//! 燒錄：`./flash.sh test-01-gp`（探針需 BOOTSEL）。LED 在閃即代表程式有跑。

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut swclk = Output::new(p.PIN_2, Level::Low); // GP2 = SWCLK（探針 pin 4）
    let mut swdio = Output::new(p.PIN_3, Level::Low); // GP3 = SWDIO（探針 pin 5）
    let mut led = Output::new(p.PIN_25, Level::Low); // 板載 LED（GP25）

    let mut n: u32 = 0;
    loop {
        swclk.toggle(); // 每 250µs → GP2 ≈ 2 kHz
        if n & 1 == 0 {
            swdio.toggle(); // 每 500µs → GP3 ≈ 1 kHz（與 GP2 可區分）
        }
        if n % 4000 == 0 {
            led.toggle(); // ~1s 心跳（程式存活指示）
        }
        n = n.wrapping_add(1);
        Timer::after(Duration::from_micros(250)).await;
    }
}
