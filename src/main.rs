//! debugprobe-rs — Raspberry Pi Debug Probe 韌體的 Rust/Embassy 重寫。
//!
//! Phase 0：Embassy 骨架 + LED 閃爍。
//! Phase 1：USB 裝置列舉（device/字串描述符、flash 序號、WinUSB BOS/MS OS 2.0）。
//! 後續 phase 會加入 SWD(CMSIS-DAP)、UART 橋接與 AutoBaud。

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::Spawner;
use portable_atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart};
use embassy_rp::usb::Driver;
use embassy_rp::{
    bind_interrupts, peripherals::PIO0, peripherals::PIO1, peripherals::UART1, peripherals::USB, usb,
};
use embassy_time::{Duration, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::driver::{Endpoint, EndpointIn, EndpointOut};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

mod autobaud;
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
    PIO1_IRQ_0 => PioInterruptHandler<PIO1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
});

// 跨 task 共享狀態，供 OLED 狀態畫面顯示（皆為非阻塞 atomic，不增加 OLED flush 頻率）。
static USB_UP: AtomicBool = AtomicBool::new(false);
static DAP_COMMANDS: AtomicU32 = AtomicU32::new(0);
static LAST_DAP_CMD: AtomicU8 = AtomicU8::new(0xFF); // 最後收到的 DAP 指令 ID
static UART_RX_BYTES: AtomicU32 = AtomicU32::new(0); // 目標→host（client log）
static UART_TX_BYTES: AtomicU32 = AtomicU32::new(0); // host→目標

/// 跑 USB 裝置主迴圈（對應 C 的 usb_thread / tud_task）。
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, usbdev::ProbeDriver>) {
    device.run().await;
}

/// CMSIS-DAP v2 傳輸 task：讀 bulk OUT → 處理 → 寫 bulk IN
/// （對應 C 的 dap_thread + tusb_edpt_handler）。
#[embassy_executor::task]
async fn dap_task(mut transport: usbdev::DapTransport, mut dap: dap::Dap<'static>) {
    use embassy_futures::select::{Either, select};
    let mut bulk_req = [0u8; 64];
    let mut hid_req = [0u8; 64];
    let mut resp = [0u8; 64];

    transport.read_ep.wait_enabled().await;
    USB_UP.store(true, Ordering::Relaxed);
    loop {
        // 同時等 v2 bulk OUT 與 v1 HID OUT，任一到達即處理。
        match select(
            transport.read_ep.read(&mut bulk_req),
            transport.hid_reader.read(&mut hid_req),
        )
        .await
        {
            // CMSIS-DAP v2 (bulk)
            Either::First(Ok(n)) if n > 0 => {
                LAST_DAP_CMD.store(bulk_req[0], Ordering::Relaxed);
                let len = dap.execute_command(&bulk_req[..n], &mut resp).await;
                DAP_COMMANDS.fetch_add(1, Ordering::Relaxed);
                if len > 0 {
                    let _ = transport.write_ep.write(&resp[..len]).await;
                }
            }
            // CMSIS-DAP v1 (HID)：報告固定 64 bytes
            Either::Second(Ok(_)) => {
                LAST_DAP_CMD.store(hid_req[0], Ordering::Relaxed);
                resp.fill(0);
                let _ = dap.execute_command(&hid_req, &mut resp).await;
                DAP_COMMANDS.fetch_add(1, Ordering::Relaxed);
                let _ = transport.hid_writer.write(&resp).await;
            }
            _ => {}
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // --- OLED 除錯顯示（I2C1: SCL=GP7, SDA=GP6）---
    let mut i2c_cfg = embassy_rp::i2c::Config::default();
    i2c_cfg.frequency = 400_000; // 400kHz fast-mode，縮短 flush 時間
    let i2c = embassy_rp::i2c::I2c::new_blocking(p.I2C1, p.PIN_7, p.PIN_6, i2c_cfg);
    let mut dbg = display::DebugOled::new(i2c);
    dbg.status(&["debugprobe-rs", "booting..."]);

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
    // 注意：OLED flush 是 blocking I2C（~10ms@400kHz）會卡住 executor，
    // 因此在 DAP 活動中（偵錯/燒錄）跳過 OLED 更新，避免打斷 DAP/UART。
    let mut last_dap = 0u32;
    loop {
        led.toggle();
        let dap = DAP_COMMANDS.load(Ordering::Relaxed);
        let dap_active = dap != last_dap;
        last_dap = dap;
        if !dap_active {
            let usb = if USB_UP.load(Ordering::Relaxed) {
                "USB connected"
            } else {
                "USB waiting"
            };
            // 最後收到的 host 指令 + UART(client log) 收發量
            let mut l_cmd: heapless::String<21> = heapless::String::new();
            let _ = write!(
                l_cmd,
                "DAP {} #{}",
                dap::cmd_name(LAST_DAP_CMD.load(Ordering::Relaxed)),
                dap
            );
            let mut l_uart: heapless::String<21> = heapless::String::new();
            let _ = write!(
                l_uart,
                "UART rx{} tx{}",
                UART_RX_BYTES.load(Ordering::Relaxed),
                UART_TX_BYTES.load(Ordering::Relaxed)
            );
            dbg.status(&["debugprobe-rs", usb, l_cmd.as_str(), l_uart.as_str()]);
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}
