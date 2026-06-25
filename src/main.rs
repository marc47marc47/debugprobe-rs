//! debugprobe-rs — Raspberry Pi Debug Probe 韌體的 Rust/Embassy 重寫。
//!
//! Phase 0：Embassy 骨架 + LED 閃爍。
//! Phase 1：USB 裝置列舉（device/字串描述符、flash 序號、WinUSB BOS/MS OS 2.0）。
//! 後續 phase 會加入 SWD(CMSIS-DAP)、UART 橋接與 AutoBaud。

#![no_std]
#![no_main]
// 最小診斷版（active-detect 關閉）：整個 OLED/偵測子系統未使用 → 抑制其 dead_code/unused_import。
// 完整版（含 active-detect）不受影響，仍維持 clippy 零警告。
#![cfg_attr(not(feature = "active-detect"), allow(dead_code, unused_imports))]

use embassy_executor::{Executor, Spawner};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart};
use embassy_rp::usb::Driver;
use embassy_rp::{
    bind_interrupts, peripherals::PIO0, peripherals::PIO1, peripherals::UART1, peripherals::USB, usb,
};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

mod autobaud;
mod board;
mod dap;
mod display;
mod logic;
mod probe;
mod serial;
#[path = "uart.rs"]
mod bridge;
#[path = "usb/mod.rs"]
mod usbdev;

// 註：RP2040 的 boot2 與 RP2350 的 image def (.start_block) 皆由 embassy-rp 自動提供。

// picotool 可讀的 binary info（對應 C 的 bi_decl / probe_config.c）。
#[unsafe(link_section = ".bi_entries")]
#[used]
static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"debugprobe-rs"),
    embassy_rp::binary_info::rp_program_description!(c"Raspberry Pi Debug Probe (Rust/Embassy)"),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    PIO1_IRQ_0 => PioInterruptHandler<PIO1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<embassy_rp::peripherals::DMA_CH0>;
});


mod chipdb;
mod state;
mod wiring;
#[cfg(feature = "active-detect")]
mod scan;
mod tasks;
use tasks::{dap_task, oled_task, usb_task};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // --- OLED 除錯顯示（I2C1: SCL=GP7, SDA=GP6）---（最小診斷版省略）
    #[cfg(feature = "active-detect")]
    let dbg = {
        let mut i2c_cfg = embassy_rp::i2c::Config::default();
        i2c_cfg.frequency = 400_000; // 400kHz fast-mode，縮短 flush 時間
        let i2c = embassy_rp::i2c::I2c::new_blocking(p.I2C1, p.PIN_7, p.PIN_6, i2c_cfg);
        let mut dbg = display::DebugOled::new(i2c);
        dbg.status(&["debugprobe-rs", "booting..."]);
        dbg
    };

    // --- 序號（flash unique ID / OTP chip id）---
    static SERIAL: StaticCell<serial::SerialString> = StaticCell::new();
    let serial = SERIAL.init(serial::read_serial(p.FLASH));

    // --- USB 裝置 ---
    let driver = Driver::new(p.USB, Irqs);
    let (device, dap_transport, cdc_class) = usbdev::build(driver, serial.as_str());
    spawner.spawn(usb_task(device).unwrap());

    // --- AutoBaud（PIO1 量測 UART RX 邊緣，魔術 baud 9728 觸發）---
    let pio1 = Pio::new(p.PIO1, Irqs);
    let ab = autobaud::AutoBaud::new(pio1.common, pio1.sm0, board::UART_RX);

    // --- UART 橋接 (UART1, GPIO4/5) ---
    let mut uart_cfg = embassy_rp::uart::Config::default();
    uart_cfg.baudrate = board::UART_BAUDRATE;
    static UART_TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static UART_RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let bridge_uart = BufferedUart::new(
        p.UART1,
        p.PIN_4,
        p.PIN_5,
        Irqs,
        UART_TX_BUF.init([0; 256]),
        UART_RX_BUF.init([0; 256]),
        uart_cfg,
    );
    spawner.spawn(bridge::uart_bridge_task(cdc_class, bridge_uart, ab).unwrap());

    // --- SWD 物理層 (PIO0/SM0) + 邏輯擷取 (PIO0/SM1 + DMA) ---
    let pio = Pio::new(p.PIO0, Irqs);
    let mut pio_common = pio.common;
    // 邏輯擷取：用 PIO0 SM1 + DMA 取樣 SWCLK/SWDIO（in_base=SWCLK，SWDIO 須為其 +1）。
    // 須在 pio_common 移入 Probe 前載入擷取程式。
    let cap_dma = embassy_rp::dma::Channel::new(p.DMA_CH0, Irqs);
    let cap = logic::LogicCapture::new(&mut pio_common, pio.sm1, cap_dma, board::PIN_SWCLK);
    #[cfg(feature = "board-debug-probe")]
    let (swclk, swdio, swdi, reset) = (
        pio_common.make_pio_pin(p.PIN_12),
        pio_common.make_pio_pin(p.PIN_14),
        Some(pio_common.make_pio_pin(p.PIN_13)),
        None,
    );
    #[cfg(any(feature = "board-pico", feature = "board-pico2"))]
    let (swclk, swdio, swdi, reset) = (
        pio_common.make_pio_pin(p.PIN_2),
        pio_common.make_pio_pin(p.PIN_3),
        None,
        Some(p.PIN_1),
    );
    let probe = probe::Probe::new(pio_common, pio.sm0, swclk, swdio, swdi, reset);

    // --- CMSIS-DAP 核心 + v2 傳輸 task ---
    let dap = dap::Dap::new(probe, serial.as_str());
    spawner.spawn(dap_task(dap_transport, dap, cap).unwrap());

    // --- 多核心 affinity：OLED+LED → core1；core0 留 USB/DAP/UART/AutoBaud ---
    // core1 的 blocking I2C flush 不影響 core0；stack 放大到 32KB 排除溢位。
    // 最小診斷版（無 active-detect）：不啟 core1、不點 LED、不跑 OLED。
    #[cfg(feature = "active-detect")]
    {
        // --- 存活指示 LED（對應 C 的 PROBE_USB_CONNECTED_LED）---
        #[cfg(feature = "board-debug-probe")]
        let led = Output::new(p.PIN_2, Level::Low);
        #[cfg(any(feature = "board-pico", feature = "board-pico2"))]
        let led = Output::new(p.PIN_25, Level::Low);

        static mut CORE1_STACK: Stack<32768> = Stack::new();
        static EXECUTOR1: StaticCell<Executor> = StaticCell::new();
        spawn_core1(
            p.CORE1,
            unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
            move || {
                let executor1 = EXECUTOR1.init(Executor::new());
                executor1.run(|s| s.spawn(oled_task(dbg, led).unwrap()));
            },
        );
    }

    // 重要：core0 的 main task 必須**不返回**。實測若 spawn_core1 後讓 main 返回，
    // core0 的 DAP AP 存取會一致失敗（DP 可讀、AP 壞）；park 住 main 即正常。
    core::future::pending::<()>().await;
}
