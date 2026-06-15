//! debugprobe-rs — Raspberry Pi Debug Probe 韌體的 Rust/Embassy 重寫。
//!
//! Phase 0：Embassy 骨架 + LED 閃爍。
//! Phase 1：USB 裝置列舉（device/字串描述符、flash 序號、WinUSB BOS/MS OS 2.0）。
//! 後續 phase 會加入 SWD(CMSIS-DAP)、UART 橋接與 AutoBaud。

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart};
use embassy_rp::usb::Driver;
use embassy_rp::{bind_interrupts, peripherals::PIO0, peripherals::UART1, peripherals::USB, usb};
use embassy_time::{Duration, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::driver::{Endpoint, EndpointIn, EndpointOut};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

mod board;
mod dap;
mod display;
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
    UART1_IRQ => BufferedInterruptHandler<UART1>;
});

// 跨 task 共享狀態，供 OLED 狀態畫面顯示。
static USB_UP: AtomicBool = AtomicBool::new(false);
static DAP_COMMANDS: AtomicU32 = AtomicU32::new(0);

/// 跑 USB 裝置主迴圈（對應 C 的 usb_thread / tud_task）。
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, usbdev::ProbeDriver>) {
    device.run().await;
}

/// CMSIS-DAP v2 傳輸 task：讀 bulk OUT → 處理 → 寫 bulk IN
/// （對應 C 的 dap_thread + tusb_edpt_handler）。
#[embassy_executor::task]
async fn dap_task(mut transport: usbdev::DapTransport, mut dap: dap::Dap<'static>) {
    let mut req = [0u8; 64];
    let mut resp = [0u8; 64];
    loop {
        transport.read_ep.wait_enabled().await;
        USB_UP.store(true, Ordering::Relaxed);
        loop {
            let n = match transport.read_ep.read(&mut req).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                continue;
            }
            let len = dap.execute_command(&req[..n], &mut resp).await;
            DAP_COMMANDS.fetch_add(1, Ordering::Relaxed);
            if len > 0 && transport.write_ep.write(&resp[..len]).await.is_err() {
                break;
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // --- OLED 除錯顯示（I2C1: SCL=GP7, SDA=GP6）---
    let i2c = embassy_rp::i2c::I2c::new_blocking(
        p.I2C1,
        p.PIN_7,
        p.PIN_6,
        embassy_rp::i2c::Config::default(),
    );
    let mut dbg = display::DebugOled::new(i2c);
    dbg.status(&["debugprobe-rs", "booting..."]);

    // --- 序號（flash unique ID / OTP chip id）---
    static SERIAL: StaticCell<serial::SerialString> = StaticCell::new();
    let serial = SERIAL.init(serial::read_serial(p.FLASH));

    // --- USB 裝置 ---
    let driver = Driver::new(p.USB, Irqs);
    let (device, dap_transport, cdc_class) = usbdev::build(driver, serial.as_str());
    spawner.spawn(usb_task(device).unwrap());

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
    spawner.spawn(bridge::uart_bridge_task(cdc_class, bridge_uart).unwrap());

    // --- SWD 物理層 (PIO0/SM0) ---
    let pio = Pio::new(p.PIO0, Irqs);
    let mut pio_common = pio.common;
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
    spawner.spawn(dap_task(dap_transport, dap).unwrap());

    // --- 存活指示 LED（對應 C 的 PROBE_USB_CONNECTED_LED）---
    #[cfg(feature = "board-debug-probe")]
    let led_pin = p.PIN_2;
    #[cfg(any(feature = "board-pico", feature = "board-pico2"))]
    let led_pin = p.PIN_25;

    let mut led = Output::new(led_pin, Level::Low);

    // --- OLED 狀態畫面 + LED 心跳 ---
    loop {
        led.toggle();
        let usb = if USB_UP.load(Ordering::Relaxed) {
            "USB: connected"
        } else {
            "USB: waiting"
        };
        let mut dapline: heapless::String<21> = heapless::String::new();
        let _ = write!(dapline, "DAP cmds: {}", DAP_COMMANDS.load(Ordering::Relaxed));
        dbg.status(&["debugprobe-rs", serial.as_str(), usb, dapline.as_str()]);
        Timer::after(Duration::from_millis(250)).await;
    }
}
