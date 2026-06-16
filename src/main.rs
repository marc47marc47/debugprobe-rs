//! debugprobe-rs — Raspberry Pi Debug Probe 韌體的 Rust/Embassy 重寫。
//!
//! Phase 0：Embassy 骨架 + LED 閃爍。
//! Phase 1：USB 裝置列舉（device/字串描述符、flash 序號、WinUSB BOS/MS OS 2.0）。
//! 後續 phase 會加入 SWD(CMSIS-DAP)、UART 橋接與 AutoBaud。

#![no_std]
#![no_main]

use core::fmt::Write as _;
use embassy_executor::{Executor, Spawner};
use portable_atomic::{AtomicU8, AtomicU32, Ordering};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::multicore::{Stack, spawn_core1};
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

// 跨 task 共享狀態（非阻塞 atomic）。事件用 token ring（環形緩衝，最新覆蓋最舊、無溢出）。
const EVT_N: usize = 3; // 環深度（配合 OLED 顯示行數）
static EVT_RING: [AtomicU8; EVT_N] = [
    AtomicU8::new(0xFF),
    AtomicU8::new(0xFF),
    AtomicU8::new(0xFF),
];
static EVT_SEQ: AtomicU32 = AtomicU32::new(0); // 單調寫入序號（單一寫入者 = dap_task）
static UART_RX_BYTES: AtomicU32 = AtomicU32::new(0); // 目標→host（client log）
static UART_TX_BYTES: AtomicU32 = AtomicU32::new(0); // host→目標
/// layer 2 目標自動偵測結果：0 = 無/未偵測；否則 bit31=有效旗標 | 低 12 位 DEV_ID。
static TARGET_DEVID: AtomicU32 = AtomicU32::new(0);
const DEVID_VALID: u32 = 1 << 31;

/// 記錄一筆 DAP 事件（dap_task 單一寫入者）。
fn record_evt(id: u8) {
    let s = EVT_SEQ.load(Ordering::Relaxed);
    EVT_RING[(s as usize) % EVT_N].store(id, Ordering::Relaxed);
    EVT_SEQ.store(s.wrapping_add(1), Ordering::Relaxed);
}

/// 取第 i 新的事件名稱（i=0 最新）；無則回空字串。
fn evt_name(i: u32) -> &'static str {
    let seq = EVT_SEQ.load(Ordering::Relaxed);
    if seq > i {
        let idx = ((seq - 1 - i) as usize) % EVT_N;
        dap::cmd_name(EVT_RING[idx].load(Ordering::Relaxed))
    } else {
        ""
    }
}

/// STM32 DBGMCU DEV_ID（12-bit）→ 晶片型號字串（供 OLED 顯示 layer 2 目標）。
fn chip_name(devid: u16) -> Option<&'static str> {
    Some(match devid {
        0x413 => "STM32F405/407",
        0x419 => "STM32F42x/43x",
        0x421 => "STM32F446",
        0x423 => "STM32F401xBC",
        0x431 => "STM32F411",
        0x433 => "STM32F401xDE",
        0x441 => "STM32F412",
        0x458 => "STM32F410",
        0x463 => "STM32F413",
        0x449 => "STM32F74x/75x",
        0x450 => "STM32H74x/75x",
        0x414 => "STM32F1 HD",
        0x410 => "STM32F1/GD32",
        0x412 => "STM32F1 LD",
        0x430 => "STM32F1 XL",
        0x415 => "STM32L4x6",
        0x435 => "STM32L43x/44x",
        _ => return None,
    })
}

/// 跑 USB 裝置主迴圈（對應 C 的 usb_thread / tud_task）。
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, usbdev::ProbeDriver>) {
    device.run().await;
}

/// CMSIS-DAP v2 傳輸 task：讀 bulk OUT → 處理 → 寫 bulk IN
/// （對應 C 的 dap_thread + tusb_edpt_handler）。
#[embassy_executor::task]
async fn dap_task(mut transport: usbdev::DapTransport, mut dap: dap::Dap<'static>) {
    use embassy_futures::select::{Either, Either3, select, select3};
    let mut bulk_req = [0u8; 64];
    let mut hid_req = [0u8; 64];
    let mut resp = [0u8; 64];
    let mut absent: u32 = 0; // 連續 ping 不到次數（拔除 hysteresis）

    const IDLE: Duration = Duration::from_millis(2000);
    loop {
        // 先確認 USB DAP 端點已啟用；wait_enabled 為 level 觸發（已啟用即立即返回）。
        // 期間若逾 2 秒「無 USB host / 未啟用」也會 idle → 仍能自主偵測晶片（不需 USB 連線）。
        let idle = match select(transport.read_ep.wait_enabled(), Timer::after(IDLE)).await {
            // USB 已啟用：等 v2 bulk / v1 HID 指令；逾 2 秒閒置則 idle。
            Either::First(()) => {
                match select3(
                    transport.read_ep.read(&mut bulk_req),
                    transport.hid_reader.read(&mut hid_req),
                    Timer::after(IDLE),
                )
                .await
                {
                    Either3::First(Ok(n)) if n > 0 => {
                        record_evt(bulk_req[0]);
                        let len = dap.execute_command(&bulk_req[..n], &mut resp).await;
                        if len > 0 {
                            let _ = transport.write_ep.write(&resp[..len]).await;
                        }
                        false
                    }
                    Either3::Second(Ok(_)) => {
                        record_evt(hid_req[0]);
                        resp.fill(0);
                        let _ = dap.execute_command(&hid_req, &mut resp).await;
                        let _ = transport.hid_writer.write(&resp).await;
                        false
                    }
                    Either3::Third(()) => true, // host 已連線但閒置
                    _ => false,
                }
            }
            // 未啟用（無 USB host）且逾時 → idle。
            Either::Second(()) => true,
        };

        // 閒置（含完全無 USB 連線）：改用「拔插」事件驅動，避免持續做完整 SWD 掃描。
        // 先用 SWDIO 電位（不發 SWD、不干擾目標）判斷是否連接；只有在「連接狀態改變」
        // 或「已連接但尚未成功讀到型號」時，才做一次完整 SWD 掃描。狀態不變則維持顯示。
        if idle {
            if dap.target_present().await {
                // 有回應：reset 拔除計數。剛插入 / 尚未成功讀到型號才做一次完整掃描；
                // 已知型號就維持顯示，不再發完整掃描（只剩上面的輕量 ping）。
                absent = 0;
                if TARGET_DEVID.load(Ordering::Relaxed) & DEVID_VALID == 0 {
                    if let Some(id) = dap.detect_target_devid().await {
                        TARGET_DEVID.store(DEVID_VALID | id as u32, Ordering::Relaxed);
                    }
                }
            } else {
                // 連續 2 次 ping 不到才視為拔除（hysteresis 防單次漏讀閃爍）。
                absent += 1;
                if absent >= 2 {
                    TARGET_DEVID.store(0, Ordering::Relaxed);
                }
            }
        }
    }
}

/// OLED 每 3 秒批次顯示，跑在 **core1**。
#[embassy_executor::task]
async fn oled_task(mut dbg: display::DebugOled, mut led: Output<'static>) {
    loop {
        led.toggle();
        let mut l_ver: heapless::String<21> = heapless::String::new();
        let _ = write!(l_ver, "debugprobe-rs {}", env!("CARGO_PKG_VERSION"));
        // 第 2 行：自動偵測的 layer 2 晶片型號（dap_task 閒置時更新）。
        let mut l_chip: heapless::String<21> = heapless::String::new();
        let dv = TARGET_DEVID.load(Ordering::Relaxed);
        if dv & DEVID_VALID != 0 {
            let id = (dv & 0xFFF) as u16;
            match chip_name(id) {
                Some(n) => {
                    let _ = write!(l_chip, "{}", n);
                }
                None => {
                    let _ = write!(l_chip, "chip 0x{:03X}", id);
                }
            }
        } else {
            let _ = write!(l_chip, "no target");
        }
        let mut l_uart: heapless::String<21> = heapless::String::new();
        let _ = write!(
            l_uart,
            "rx:{} tx:{}",
            UART_RX_BYTES.load(Ordering::Relaxed),
            UART_TX_BYTES.load(Ordering::Relaxed)
        );
        dbg.status(&[
            l_ver.as_str(),
            l_chip.as_str(),
            evt_name(0),
            evt_name(1),
            l_uart.as_str(),
        ]);
        Timer::after(Duration::from_millis(3000)).await;
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
    let led = Output::new(p.PIN_2, Level::Low);
    #[cfg(any(feature = "board-pico", feature = "board-pico2"))]
    let led = Output::new(p.PIN_25, Level::Low);

    // --- 多核心 affinity：OLED+LED → core1；core0 留 USB/DAP/UART/AutoBaud ---
    // core1 的 blocking I2C flush 不影響 core0；stack 放大到 32KB 排除溢位。
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

    // 重要：core0 的 main task 必須**不返回**。實測若 spawn_core1 後讓 main 返回，
    // core0 的 DAP AP 存取會一致失敗（DP 可讀、AP 壞）；park 住 main 即正常。
    core::future::pending::<()>().await;
}
