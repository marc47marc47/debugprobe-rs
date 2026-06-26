//! Embassy tasks：usb_task / dap_task / oled_task。自 main.rs 抽出（Phase 13 R5）。
use crate::chipdb::{chip_name, core_name, vendor_name};
#[cfg(feature = "active-detect")]
use crate::scan::idle_scan;
use crate::state::{TARGET, WAVE, record_evt};
use crate::{dap, display, logic, usbdev};
use core::fmt::Write as _;
use embassy_rp::gpio::Output;
use embassy_time::{Duration, Timer};
use embassy_usb::UsbDevice;
use embassy_usb::driver::{Endpoint, EndpointIn, EndpointOut};

/// 跑 USB 裝置主迴圈（對應 C 的 usb_thread / tud_task）。
#[embassy_executor::task]
pub(crate) async fn usb_task(mut device: UsbDevice<'static, usbdev::ProbeDriver>) {
    device.run().await;
}

/// CMSIS-DAP v2 傳輸 task：讀 bulk OUT → 處理 → 寫 bulk IN
/// （對應 C 的 dap_thread + tusb_edpt_handler）。
#[embassy_executor::task]
// 最小診斷版（無 active-detect）：cap/absent/sticky_khz 不使用，僅作純指令轉發。
#[cfg_attr(not(feature = "active-detect"), allow(unused_variables, unused_mut))]
pub(crate) async fn dap_task(
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
    let mut scan_st = crate::scan::ScanState::new(); // 黏著速率/拔除遲滯/鬆動統計持久狀態
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
                idle_scan(&mut dap, &mut cap, &mut scan_st).await;
            }
        }
    }
}

/// OLED 顯示，跑在 **core1**：上 3 行文字（版本/晶片/可燒狀態）+ 下半 SWD 訊號品質即時波形。
#[embassy_executor::task]
pub(crate) async fn oled_task(mut dbg: display::DebugOled, mut led: Output<'static>) {
    // 第 5 行：偵測到新鬆動後只顯示 flap 約 3 秒(FLAP_HOLD×250ms)，之後回到 S/T 時脈（計數照常累加）。
    const FLAP_HOLD: u8 = 12;
    let mut prev_flaps = (0u32, 0u32);
    let mut flap_hold: u8 = 0;
    loop {
        led.toggle();
        let valid = TARGET.valid();
        let verdict = TARGET.verdict();
        let lines = TARGET.lines(); // 逐線連通（走線監測）
        let (dio_c, clk_c) = (lines.dio, lines.clk);
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
        // 第 5 行：剛偵測到新鬆動 → 顯示 flpC/D 約 3 秒（拔插測試用）；否則顯示「穩定clk + 測試clk」。
        // S = 已確認 AP 穩的速率(clamp/操作基準)；T = 本輪正在用/往上試的速率。
        let mut l_scale: heapless::String<21> = heapless::String::new();
        let (cf, df) = TARGET.flaps();
        if cf + df > prev_flaps.0 + prev_flaps.1 {
            flap_hold = FLAP_HOLD; // 有新鬆動 → 顯示幾秒
        }
        prev_flaps = (cf, df);
        if flap_hold > 0 {
            flap_hold -= 1;
            let _ = write!(l_scale, "flpC{} D{}", cf, df);
        } else {
            let _ = write!(l_scale, "S{}k T{}k", TARGET.stable_khz(), TARGET.used_khz());
        }
        // 右側 4 條柱狀圖：逐線連通 C/D（probe_lines，0/1）+ 連線品質 DP/AP（0..16）。
        // 只用可信指標：Ce/De/hC/hD 來自對不準的擷取窗、會誤導，已不顯示。
        let q = TARGET.link();
        let (dp, ap) = (q.dp as u32, q.ap as u32);
        let bars = [
            display::Bar { label: "C", value: lines.clk as u32, max: 1 },
            display::Bar { label: "D", value: lines.dio as u32, max: 1 },
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
