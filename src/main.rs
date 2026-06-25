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

use core::fmt::Write as _;
use embassy_executor::{Executor, Spawner};
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
use chipdb::*;
use state::*;
use wiring::*;

/// 跑 USB 裝置主迴圈（對應 C 的 usb_thread / tud_task）。
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, usbdev::ProbeDriver>) {
    device.run().await;
}

/// CMSIS-DAP v2 傳輸 task：讀 bulk OUT → 處理 → 寫 bulk IN
/// （對應 C 的 dap_thread + tusb_edpt_handler）。
#[embassy_executor::task]
// 最小診斷版（無 active-detect）：cap/absent/sticky_khz 不使用，僅作純指令轉發。
#[cfg_attr(not(feature = "active-detect"), allow(unused_variables, unused_mut))]
async fn dap_task(
    mut transport: usbdev::DapTransport,
    mut dap: dap::Dap<'static>,
    mut cap: logic::LogicCapture<'static>,
) {
    use embassy_futures::select::{Either, Either3, select, select3};
    #[cfg(feature = "wiring-monitor")]
    use embassy_time::Instant;
    let mut bulk_req = [0u8; 64];
    let mut hid_req = [0u8; 64];
    let mut resp = [0u8; 64];
    #[cfg(feature = "active-detect")]
    let mut absent: u32 = 0; // 連續取樣不到次數（拔除 hysteresis）
    #[cfg(feature = "active-detect")]
    let mut sticky_khz: u32 = 1000; // 黏著速率：鎖在上次能通的 SWCLK，避免每輪重掃造成顯示亂跳
    #[cfg(feature = "active-detect")]
    let mut prev_lines: Option<(bool, bool)> = None; // 上輪逐線連通（鬆動 flap 統計用）
    // 走線監測：最後一次收到 host DAP 指令的時間；GUARD 內不監測，保護 F1 燒錄不被擾。
    #[cfg(feature = "wiring-monitor")]
    let mut last_host: Option<Instant> = None;
    #[cfg(feature = "wiring-monitor")]
    const GUARD: Duration = Duration::from_secs(2);

    // FAST：無 USB host（未列舉）時的取樣節奏（每 FAST 即進行一次自主偵測）。
    // SLOW：有 host 時的指令等待逾時。host 在線時**一律不**插入自主偵測（讓出 SWD 給除錯器，
    //       避免 line reset/掃頻/改寫 DP 暫存器在 host 兩次存取間清掉鏈路狀態 → 不穩定）。
    const FAST: Duration = Duration::from_millis(250);
    const SLOW: Duration = Duration::from_millis(300);
    loop {
        // wait_enabled 為 level 觸發（已啟用即立即返回）。
        // 無 USB host → 每 FAST 即 idle（即時波形）；有 host → 進指令迴圈，僅 host 閒置 SLOW 才 idle。
        let idle = match select(transport.read_ep.wait_enabled(), Timer::after(FAST)).await {
            Either::First(()) => {
                match select3(
                    transport.read_ep.read(&mut bulk_req),
                    transport.hid_reader.read(&mut hid_req),
                    Timer::after(SLOW),
                )
                .await
                {
                    Either3::First(Ok(n)) if n > 0 => {
                        record_evt(bulk_req[0]);
                        #[cfg(feature = "wiring-monitor")]
                        {
                            last_host = Some(Instant::now());
                        }
                        let len = dap.execute_command(&bulk_req[..n], &mut resp).await;
                        if len > 0 {
                            let _ = transport.write_ep.write(&resp[..len]).await;
                        }
                        false
                    }
                    Either3::Second(Ok(_)) => {
                        record_evt(hid_req[0]);
                        #[cfg(feature = "wiring-monitor")]
                        {
                            last_host = Some(Instant::now());
                        }
                        resp.fill(0);
                        let _ = dap.execute_command(&hid_req, &mut resp).await;
                        let _ = transport.hid_writer.write(&resp).await;
                        false
                    }
                    // host 已連線但閒置：
                    // - 正常(穩定)版：**一律不偵測**。自主偵測會做 line reset + 掃頻 + 改寫 DP
                    //   SELECT/CTRL-STAT，在 host 兩次存取間清掉鏈路 → 忽好忽壞。host 在線時讓出 SWD。
                    // - force-detect / wiring-monitor 診斷版：插著 PC 也偵測（wiring-monitor 另有 GUARD 退避）。
                    #[cfg(any(feature = "force-detect", feature = "wiring-monitor"))]
                    Either3::Third(()) => true,
                    #[cfg(not(any(feature = "force-detect", feature = "wiring-monitor")))]
                    Either3::Third(()) => false,
                    _ => false,
                }
            }
            // 未啟用（無 USB host）且逾時 → idle。
            Either::Second(()) => true,
        };

        // 最小診斷版：不做任何主動 SWD 動作（core0 僅轉發 host 指令）。
        #[cfg(not(feature = "active-detect"))]
        let _ = idle;

        // 閒置（含無 USB 連線）：跑一輪自主掃描（逐線連通 + 擷取波形 + 偵測晶片 + 量連線品質）。
        // wiring-monitor：剛收到 host 指令 GUARD 內**跳過**，確保不在燒錄封包間隙插入而毀掉 session。
        #[cfg(feature = "active-detect")]
        if idle {
            #[cfg(feature = "wiring-monitor")]
            let proceed = last_host.is_none_or(|t| Instant::now().duration_since(t) >= GUARD);
            #[cfg(not(feature = "wiring-monitor"))]
            let proceed = true;
            if proceed {
                idle_scan(&mut dap, &mut cap, &mut sticky_khz, &mut absent, &mut prev_lines).await;
            }
        }
    }
}

#[cfg(feature = "active-detect")]
async fn adaptive_sweep(dap: &mut dap::Dap<'static>, sticky: &mut u32) -> u32 {
    // 純 single-drop（只讀 DPIDR、**絕不寫 TARGETSEL**）：先試黏著速率，再由快到慢掃。
    // 不做 multidrop——TARGETSEL 會把 DPv2 STM32 誤 deselect 且不可逆，整顆從此偵測不到。
    TARGET.set_probe_khz(*sticky);
    dap.set_swclk_khz(*sticky);
    dap.swd_wakeup().await;
    if dap.swd_read_dpidr().await {
        return *sticky;
    }
    for &khz in &[1000u32, 500, 250, 100, 50, 20] {
        TARGET.set_probe_khz(khz);
        dap.set_swclk_khz(khz);
        dap.swd_wakeup().await;
        if dap.swd_read_dpidr().await {
            *sticky = khz; // 鎖定新速率
            return khz;
        }
    }
    0
}

/// host 閒置時的一輪自主掃描：自適應掃頻 → 擷取波形 → 偵測晶片 → 量連線品質；更新 `TARGET`/`WAVE`。
/// 全程 save/restore SWCLK，不留痕跡給 host。
#[cfg(feature = "active-detect")]
async fn idle_scan(
    dap: &mut dap::Dap<'static>,
    cap: &mut logic::LogicCapture<'static>,
    sticky: &mut u32,
    absent: &mut u32,
    prev: &mut Option<(bool, bool)>,
) {
    use embassy_futures::select::select;
    let saved_khz = dap.swclk_khz();

    // 走線監測第一步：逐線連通（drive 反向→釋放→讀目標內部 pull）。判斷哪條線實體斷掉。
    let (dio, clk) = dap.probe_lines().await;
    TARGET.set_lines(dio, clk);
    // 鬆動統計：與上輪比較，連通狀態翻轉即累加（反覆拔插時，flap 最多者 = 最會鬆的線）。
    if let Some((pd, pc)) = *prev {
        if dio != pd {
            TARGET.bump_dio_flap();
        }
        if clk != pc {
            TARGET.bump_clk_flap();
        }
    }
    *prev = Some((dio, clk));

    let used = adaptive_sweep(dap, sticky).await;
    TARGET.set_used_khz(used);

    let mut buf = [0u32; logic::CAP_WORDS];
    let ce;
    let (dp, ap);
    if used != 0 {
        *absent = 0;
        // 偵測到目標(可用速率)才擷取波形 + 量邊緣(此時 SWCLK 在跑、量得到真訊號)。
        cap.start();
        let xfer = cap.dma_into(&mut buf);
        let _ = dap.swd_read_dpidr().await; // 訊號刺激
        let _ = select(xfer, Timer::after(Duration::from_millis(20))).await;
        cap.stop();
        let (ce0, de, ch, dh) = count_signal(&buf);
        TARGET.set_signal(ce0, de, ch, dh);
        WAVE.push(&buf);
        // 晶片偵測只在尚未鎖定時做一次（F2 功能保留）。
        if !TARGET.valid()
            && let Some(info) = dap.detect_target().await
        {
            TARGET.store(&info);
        }
        let q = dap.link_quality().await;
        TARGET.set_link(&q);
        ce = ce0;
        dp = q.dp as u32;
        ap = q.ap as u32;
    } else {
        // 無目標：不擷取(低速擷取無意義)、推平線、歸零。
        WAVE.push_flat();
        TARGET.set_signal(0, 0, 0, 0);
        TARGET.set_link(&dap::LinkQuality { dp: 0, ap: 0 });
        *absent += 1;
        if *absent >= 2 {
            TARGET.clear();
        }
        ce = 0;
        dp = 0;
        ap = 0;
    }
    // 走線判定：彙整逐線連通 + 連線品質 → 結論（供 OLED 即時顯示哪條線壞）。
    // captured = 本輪是否真的擷取了波形（used!=0）；只有此時 ce 才有意義。
    TARGET.set_verdict(classify(dio, clk, used != 0, ce, dp, ap));
    dap.set_swclk_khz(saved_khz); // 還原 host 設定
}

/// 數擷取窗內 SWCLK/SWDIO 的邊緣(跳變)數與高電位取樣數。回 (clk_e, dio_e, clk_hi, dio_hi)。
/// CLK 由探針自驅 → CLK 邊緣=0 代表探針沒驅動出 SWCLK(GP2 pad 死);邊緣=0 配高取樣數可判卡高/卡低。
#[cfg(feature = "active-detect")]
fn count_signal(buf: &[u32]) -> (u32, u32, u32, u32) {
    let (mut pc, mut pd) = logic::sample_at(buf, 0);
    let (mut ce, mut de) = (0u32, 0u32);
    let (mut ch, mut dh) = (pc as u32, pd as u32); // 取樣 0 的高電位計入
    for i in 1..logic::SAMPLES {
        let (c, d) = logic::sample_at(buf, i);
        if c != pc {
            ce += 1;
            pc = c;
        }
        if d != pd {
            de += 1;
            pd = d;
        }
        if c {
            ch += 1;
        }
        if d {
            dh += 1;
        }
    }
    (ce, de, ch, dh)
}

/// OLED 顯示，跑在 **core1**：上 3 行文字（版本/晶片/可燒狀態）+ 下半 SWD 訊號品質即時波形。
#[embassy_executor::task]
async fn oled_task(mut dbg: display::DebugOled, mut led: Output<'static>) {
    loop {
        led.toggle();
        let valid = TARGET.valid();
        let verdict = TARGET.verdict();
        let (dio_c, clk_c) = TARGET.lines(); // 逐線連通（走線監測）
        // 走線正常(OK/未判定)→ 照常顯示晶片資訊(F2)；有走線問題 → 顯示哪條線壞。
        let show_chip = verdict.shows_chip();
        // 第 1 行：晶片型號 或 走線結論。
        let mut l_chip: heapless::String<21> = heapless::String::new();
        if show_chip && valid {
            let id = TARGET.devid();
            let designer = TARGET.designer();
            let part = TARGET.part();
            let core = TARGET.core();
            // 顯示優先序：精確型號 → nRF → 廠商+核心 → 核心 → 廠商 → DP/core 後援。
            if id != 0 {
                // ST / GD32：精確型號（查表中則型號，否則 DEV_ID）。
                match chip_name(id) {
                    Some(n) => {
                        let _ = write!(l_chip, "{}", n);
                    }
                    None => {
                        let _ = write!(l_chip, "STM32 0x{:03X}", id);
                    }
                }
            } else if (0x52000..=0x55000).contains(&part) {
                // Nordic nRF（FICR part 如 0x52832）
                let _ = write!(l_chip, "nRF{:X}", part);
            } else {
                // 未知型號：盡量顯示「廠商 + 核心」（A：CPUID 通用核心後援）。
                let _ = match (vendor_name(designer), core_name(core)) {
                    (Some(v), Some(c)) => write!(l_chip, "{} {}", v, c),
                    (None, Some(c)) => write!(l_chip, "{}", c),
                    (Some(v), None) => write!(l_chip, "{}", v),
                    (None, None) if core != 0 => write!(l_chip, "core 0x{:03X}", core),
                    (None, None) => write!(l_chip, "vendor 0x{:03X}", designer),
                };
            }
        } else if show_chip {
            let _ = write!(l_chip, "no target");
        } else {
            // 走線問題：顯示結論（SWCLK BAD / SWDIO BAD / GND BAD / PWR fail …）。
            let _ = write!(l_chip, "{}", verdict.text());
        }
        // 第 2 行：可燒狀態+頻率（走線好）或 逐線 OK/X（走線壞）。
        let mut l_flash: heapless::String<21> = heapless::String::new();
        if show_chip && valid {
            // 短標 + 頻率，控制在 x<82 不撞右側柱狀圖（如 "RDP0 1000k"）。
            let _ = write!(l_flash, "{} {}k", TARGET.rdp().short(), TARGET.used_khz());
        } else if show_chip {
            let _ = write!(l_flash, "probe {}k", TARGET.probe_khz());
        } else {
            let _ = write!(
                l_flash,
                "CLK:{} DIO:{}",
                if clk_c { "OK" } else { "X " },
                if dio_c { "OK" } else { "X " }
            );
        }
        let clk = WAVE.load_clk();
        let dio = WAVE.load_dio();
        let pos = WAVE.pos();
        // 左下角文字：鬆動統計（反覆拔插時哪條線最會鬆）；尚無翻轉則顯示取樣率。
        let mut l_scale: heapless::String<21> = heapless::String::new();
        let (cf, df) = TARGET.flaps();
        if cf != 0 || df != 0 {
            let _ = write!(l_scale, "flpC{} D{}", cf, df);
        } else {
            let _ = write!(l_scale, "{}ns/col", logic::sample_ns());
        }
        // 右側 6 條柱狀圖（含 line1/line2 右側）：訊號層 Ce/De/hC/hD + 連線層 DP/AP。
        // Ce0 hC0=SWCLK卡低、Ce0 hC100=卡高、Ce 有長 hC≈50=正常 toggle；DP/AP 往滿=連線品質好。
        let (ce, de, ch, dh) = TARGET.signal();
        let (dp, ap) = TARGET.link();
        let s = logic::SAMPLES as u32;
        let bars = [
            display::Bar { label: "Ce", value: ce, max: 64 },
            display::Bar { label: "De", value: de, max: 64 },
            display::Bar { label: "hC", value: ch * 100 / s, max: 100 },
            display::Bar { label: "hD", value: dh * 100 / s, max: 100 },
            display::Bar { label: "DP", value: dp, max: 16 },
            display::Bar { label: "AP", value: ap, max: 16 },
        ];
        dbg.render(&display::OledModel {
            chip: l_chip.as_str(),
            flash: l_flash.as_str(),
            clk,
            dio,
            pos,
            scale: l_scale.as_str(),
            bars: &bars,
        });
        Timer::after(Duration::from_millis(250)).await;
    }
}

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
