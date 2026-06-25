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
/// 走線健康判定結論（走線監測：由 `classify` 依逐線連通 + 訊號邊緣 + 連線品質產生）。
/// `Unknown` 為初值（尚未掃過，例如 host 在線且預設 build 不監測）→ OLED 沿用既有顯示。
#[derive(Clone, Copy)]
enum WireVerdict {
    Unknown,      // 0：尚未判定
    Ok,           // 1：兩線連通、DP/AP 近滿
    SwclkOpen,    // 2：SWCLK 斷/接觸不良（不連通或探針驅出後幾乎無邊緣）
    SwdioOpen,    // 3：SWDIO 斷
    BothOpen,     // 4：兩線皆斷（或目標整個浮空/無電）
    NoTarget,     // 5：兩線連通、SWCLK 在動，但完全讀不到 DP（對端無晶片）
    PwrParasitic, // 6：DP 穩但 AP 全失敗（寄生供電/電不足驅 AHB，或 RDP1）
    GndBad,       // 7：讀得到但 DP 成功率低（共地阻抗高、訊號抖）
    Unstable,     // 8：連上但 AP 長交易抖（邊緣/反射/接點劣化）
}

impl WireVerdict {
    fn to_u8(self) -> u8 {
        match self {
            WireVerdict::Unknown => 0,
            WireVerdict::Ok => 1,
            WireVerdict::SwclkOpen => 2,
            WireVerdict::SwdioOpen => 3,
            WireVerdict::BothOpen => 4,
            WireVerdict::NoTarget => 5,
            WireVerdict::PwrParasitic => 6,
            WireVerdict::GndBad => 7,
            WireVerdict::Unstable => 8,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => WireVerdict::Ok,
            2 => WireVerdict::SwclkOpen,
            3 => WireVerdict::SwdioOpen,
            4 => WireVerdict::BothOpen,
            5 => WireVerdict::NoTarget,
            6 => WireVerdict::PwrParasitic,
            7 => WireVerdict::GndBad,
            8 => WireVerdict::Unstable,
            _ => WireVerdict::Unknown,
        }
    }
    /// OLED 第 1 行的走線結論字串（有走線問題時取代晶片名）。
    fn text(self) -> &'static str {
        match self {
            WireVerdict::Unknown => "no target",
            WireVerdict::Ok => "OK",
            WireVerdict::SwclkOpen => "SWCLK BAD",
            WireVerdict::SwdioOpen => "SWDIO BAD",
            WireVerdict::BothOpen => "BOTH OPEN",
            WireVerdict::NoTarget => "no target",
            WireVerdict::PwrParasitic => "PWR/AP fail",
            WireVerdict::GndBad => "GND BAD",
            WireVerdict::Unstable => "UNSTABLE",
        }
    }
    /// 「走線正常、可照常顯示晶片資訊」的狀態（OK 或尚未判定）。
    fn shows_chip(self) -> bool {
        matches!(self, WireVerdict::Ok | WireVerdict::Unknown)
    }
}

/// layer 2 目標自動偵測 + 連線品質的跨 task 共享狀態（dap_task 單一寫入、oled_task 讀；全 atomic）。
struct TargetShared {
    /// 0 = 無/未偵測；否則 bit31 有效旗標 | 低 12 位 DEV_ID。
    devid: AtomicU32,
    /// 可燒錄狀態（`RdpLevel` as u8）：0=L0,1=L1,2=L2,0xFF=未知/無。
    flash: AtomicU8,
    /// JEP106 廠商碼 + 廠商專屬 part（Nordic FICR 等）。
    designer: AtomicU32,
    part: AtomicU32,
    /// CPUID PARTNO（Cortex-M 核心型號）；0=未知。
    core: AtomicU32,
    /// 自適應偵測實際採用的 SWCLK（kHz）；0 = 沒讀到。
    used_khz: AtomicU32,
    /// 連線品質訊號儀：每輪 16 次讀取成功數（DP 短交易 / AP AHB 長交易）。
    link_dp: AtomicU32,
    link_ap: AtomicU32,
    /// 目前正在嘗試的偵測 SWCLK（kHz）：掃頻每步更新。OLED 無目標時顯示，反映掃到哪個頻率。
    probe_khz: AtomicU32,
    /// 上一窗擷取的 SWCLK / SWDIO 邊緣(跳變)數。SWCLK 由探針自驅 → CLK e=0 即探針輸出死。
    clk_edges: AtomicU32,
    dio_edges: AtomicU32,
    /// SM1 取樣:該窗 SWCLK / SWDIO 為高的取樣數(0..SAMPLES)。反映電位/duty:
    /// 邊緣=0 且 高=滿 → 卡高;邊緣=0 且 高=0 → 卡低;邊緣多且 高≈半 → 正常 toggle。
    clk_hi: AtomicU32,
    dio_hi: AtomicU32,
    /// 走線監測：逐線連通（probe_lines 結果，0/1）。
    dio_conn: AtomicU8,
    clk_conn: AtomicU8,
    /// 走線判定結論（`WireVerdict` as u8）。
    verdict: AtomicU8,
    /// 鬆動統計：連通狀態翻轉（接↔斷）累計次數，最多者 = 最會鬆的線。
    clk_flaps: AtomicU32,
    dio_flaps: AtomicU32,
}

impl TargetShared {
    const VALID: u32 = 1 << 31;
    const fn new() -> Self {
        Self {
            devid: AtomicU32::new(0),
            flash: AtomicU8::new(0xFF),
            designer: AtomicU32::new(0),
            part: AtomicU32::new(0),
            core: AtomicU32::new(0),
            used_khz: AtomicU32::new(0),
            link_dp: AtomicU32::new(0),
            link_ap: AtomicU32::new(0),
            probe_khz: AtomicU32::new(0),
            clk_edges: AtomicU32::new(0),
            dio_edges: AtomicU32::new(0),
            clk_hi: AtomicU32::new(0),
            dio_hi: AtomicU32::new(0),
            dio_conn: AtomicU8::new(0),
            clk_conn: AtomicU8::new(0),
            verdict: AtomicU8::new(0),
            clk_flaps: AtomicU32::new(0),
            dio_flaps: AtomicU32::new(0),
        }
    }
    /// 記錄逐線連通結果。
    fn set_lines(&self, dio: bool, clk: bool) {
        self.dio_conn.store(dio as u8, Ordering::Relaxed);
        self.clk_conn.store(clk as u8, Ordering::Relaxed);
    }
    /// 回 (dio_connected, clk_connected)。
    fn lines(&self) -> (bool, bool) {
        (
            self.dio_conn.load(Ordering::Relaxed) != 0,
            self.clk_conn.load(Ordering::Relaxed) != 0,
        )
    }
    fn set_verdict(&self, v: WireVerdict) {
        self.verdict.store(v.to_u8(), Ordering::Relaxed);
    }
    fn verdict(&self) -> WireVerdict {
        WireVerdict::from_u8(self.verdict.load(Ordering::Relaxed))
    }
    fn bump_clk_flap(&self) {
        self.clk_flaps.fetch_add(1, Ordering::Relaxed);
    }
    fn bump_dio_flap(&self) {
        self.dio_flaps.fetch_add(1, Ordering::Relaxed);
    }
    /// 回 (clk_flaps, dio_flaps)。
    fn flaps(&self) -> (u32, u32) {
        (
            self.clk_flaps.load(Ordering::Relaxed),
            self.dio_flaps.load(Ordering::Relaxed),
        )
    }
    /// 記錄掃頻目前嘗試的速率（kHz）。
    fn set_probe_khz(&self, khz: u32) {
        self.probe_khz.store(khz, Ordering::Relaxed);
    }
    fn probe_khz(&self) -> u32 {
        self.probe_khz.load(Ordering::Relaxed)
    }
    /// 記錄上一窗 SWCLK/SWDIO 的邊緣數與高電位取樣數。
    fn set_signal(&self, clk_e: u32, dio_e: u32, clk_hi: u32, dio_hi: u32) {
        self.clk_edges.store(clk_e, Ordering::Relaxed);
        self.dio_edges.store(dio_e, Ordering::Relaxed);
        self.clk_hi.store(clk_hi, Ordering::Relaxed);
        self.dio_hi.store(dio_hi, Ordering::Relaxed);
    }
    /// 回 (clk邊緣, dio邊緣, clk高取樣, dio高取樣)。
    fn signal(&self) -> (u32, u32, u32, u32) {
        (
            self.clk_edges.load(Ordering::Relaxed),
            self.dio_edges.load(Ordering::Relaxed),
            self.clk_hi.load(Ordering::Relaxed),
            self.dio_hi.load(Ordering::Relaxed),
        )
    }
    /// 寫入偵測結果（designer/part/flash 先寫，devid 含有效旗標最後寫）。
    fn store(&self, info: &dap::TargetInfo) {
        self.designer.store(info.designer as u32, Ordering::Relaxed);
        self.part.store(info.part, Ordering::Relaxed);
        self.core.store(info.core as u32, Ordering::Relaxed);
        self.flash.store(info.rdp.to_u8(), Ordering::Relaxed);
        self.devid
            .store(Self::VALID | info.devid as u32, Ordering::Relaxed);
    }
    /// 清除（無目標）。
    fn clear(&self) {
        self.devid.store(0, Ordering::Relaxed);
        self.flash.store(0xFF, Ordering::Relaxed);
    }
    fn valid(&self) -> bool {
        self.devid.load(Ordering::Relaxed) & Self::VALID != 0
    }
    fn devid(&self) -> u16 {
        (self.devid.load(Ordering::Relaxed) & 0xFFF) as u16
    }
    fn designer(&self) -> u16 {
        self.designer.load(Ordering::Relaxed) as u16
    }
    fn part(&self) -> u32 {
        self.part.load(Ordering::Relaxed)
    }
    fn core(&self) -> u16 {
        self.core.load(Ordering::Relaxed) as u16
    }
    fn rdp(&self) -> dap::RdpLevel {
        dap::RdpLevel::from_u8(self.flash.load(Ordering::Relaxed))
    }
    fn set_used_khz(&self, khz: u32) {
        self.used_khz.store(khz, Ordering::Relaxed);
    }
    fn used_khz(&self) -> u32 {
        self.used_khz.load(Ordering::Relaxed)
    }
    fn set_link(&self, q: &dap::LinkQuality) {
        self.link_dp.store(q.dp as u32, Ordering::Relaxed);
        self.link_ap.store(q.ap as u32, Ordering::Relaxed);
    }
    /// (dp, ap) 成功數。
    fn link(&self) -> (u32, u32) {
        (
            self.link_dp.load(Ordering::Relaxed),
            self.link_ap.load(Ordering::Relaxed),
        )
    }
}

static TARGET: TargetShared = TargetShared::new();

/// SWD 數位邏輯波形 token ring（環形緩衝、捲動式）：`WAVE_COLS` 欄各 1 bit，兩通道(SWCLK/SWDIO)。
/// 每輪擷取把新片段「推進」ring（最新覆蓋最舊、不積壓），OLED 由 pos 起讀（最舊→最新）→ 視覺流動。
const WAVE_COLS: usize = 128;
const WAVE_PUSH: usize = 32; // 每輪推進的欄數（捲動速度）

struct WaveRing {
    clk: [AtomicU32; 4],
    dio: [AtomicU32; 4],
    pos: AtomicU32, // 下一個寫入欄（= 最舊欄）
}

impl WaveRing {
    const fn new() -> Self {
        Self {
            clk: [const { AtomicU32::new(0) }; 4],
            dio: [const { AtomicU32::new(0) }; 4],
            pos: AtomicU32::new(0),
        }
    }
    fn set_bit(arr: &[AtomicU32; 4], col: usize, bit: bool) {
        let wi = col / 32;
        let m = 1u32 << (col % 32);
        let mut v = arr[wi].load(Ordering::Relaxed);
        v = if bit { v | m } else { v & !m };
        arr[wi].store(v, Ordering::Relaxed); // 單一寫入者(dap_task)，load/store 即可
    }
    /// 從擷取緩衝找出時脈片段，取乾淨的 WAVE_PUSH 欄(1:1，不混疊)推進 ring。
    fn push(&self, buf: &[u32]) {
        let total = logic::SAMPLES;
        // 找第一個 SWCLK 跳變，從略前處取片段（確保含時脈、非開頭閒置）。
        let (c0, _) = logic::sample_at(buf, 0);
        let mut start = 0usize;
        for i in 1..total {
            if logic::sample_at(buf, i).0 != c0 {
                start = i.saturating_sub(2);
                break;
            }
        }
        if start + WAVE_PUSH > total {
            start = total - WAVE_PUSH;
        }
        let mut pos = self.pos.load(Ordering::Relaxed) as usize;
        for k in 0..WAVE_PUSH {
            let (c, d) = logic::sample_at(buf, start + k);
            Self::set_bit(&self.clk, pos, c);
            Self::set_bit(&self.dio, pos, d);
            pos = (pos + 1) % WAVE_COLS;
        }
        self.pos.store(pos as u32, Ordering::Relaxed);
    }
    /// 無目標：推進 WAVE_PUSH 欄平線（平段捲入畫面反映「沒訊號」）。
    fn push_flat(&self) {
        let mut pos = self.pos.load(Ordering::Relaxed) as usize;
        for _ in 0..WAVE_PUSH {
            Self::set_bit(&self.clk, pos, false);
            Self::set_bit(&self.dio, pos, false);
            pos = (pos + 1) % WAVE_COLS;
        }
        self.pos.store(pos as u32, Ordering::Relaxed);
    }
    fn snapshot(arr: &[AtomicU32; 4]) -> [u32; 4] {
        [
            arr[0].load(Ordering::Relaxed),
            arr[1].load(Ordering::Relaxed),
            arr[2].load(Ordering::Relaxed),
            arr[3].load(Ordering::Relaxed),
        ]
    }
    fn load_clk(&self) -> [u32; 4] {
        Self::snapshot(&self.clk)
    }
    fn load_dio(&self) -> [u32; 4] {
        Self::snapshot(&self.dio)
    }
    fn pos(&self) -> usize {
        self.pos.load(Ordering::Relaxed) as usize
    }
}

static WAVE: WaveRing = WaveRing::new();

/// 記錄一筆 DAP 事件（dap_task 單一寫入者）。
fn record_evt(id: u8) {
    let s = EVT_SEQ.load(Ordering::Relaxed);
    EVT_RING[(s as usize) % EVT_N].store(id, Ordering::Relaxed);
    EVT_SEQ.store(s.wrapping_add(1), Ordering::Relaxed);
}

/// 取第 i 新的事件名稱（i=0 最新）；無則回空字串。
/// （OLED 改為波形圖後暫不顯示事件；保留供日後用。）
#[allow(dead_code)]
fn evt_name(i: u32) -> &'static str {
    let seq = EVT_SEQ.load(Ordering::Relaxed);
    if seq > i {
        let idx = ((seq - 1 - i) as usize) % EVT_N;
        dap::cmd_name(EVT_RING[idx].load(Ordering::Relaxed))
    } else {
        ""
    }
}

/// STM32/GD32 DBGMCU DEV_ID（12-bit）→ 晶片型號（供 OLED 顯示 layer 2 目標）。
/// 查表式（`(dev_id, name)`）；涵蓋市面常見系列；GD32F1 與 STM32F1 共用 DEV_ID 故並列標示。
static CHIP_NAMES: &[(u16, &str)] = &[
    // F0
    (0x440, "STM32F030/05x"), (0x444, "STM32F03x"), (0x442, "STM32F09x"),
    (0x445, "STM32F04x"), (0x448, "STM32F07x"),
    // F1 / GD32F1
    (0x410, "STM32F1/GD32"), (0x412, "STM32F1 LD"), (0x414, "STM32F1/GD32 HD"),
    (0x418, "STM32F1 CL"), (0x420, "STM32F1 VL"), (0x428, "STM32F1 VL-HD"), (0x430, "STM32F1 XL"),
    // F2
    (0x411, "STM32F2"),
    // F3
    (0x422, "STM32F302/303"), (0x432, "STM32F37x"), (0x438, "STM32F334"),
    (0x439, "STM32F301/302"), (0x446, "STM32F303xE"),
    // F4
    (0x413, "STM32F405/407"), (0x419, "STM32F42x/43x"), (0x421, "STM32F446"),
    (0x423, "STM32F401xBC"), (0x431, "STM32F411"), (0x433, "STM32F401xDE"),
    (0x434, "STM32F469/479"), (0x441, "STM32F412"), (0x458, "STM32F410"), (0x463, "STM32F413"),
    // F7
    (0x449, "STM32F74x/75x"), (0x451, "STM32F76x/77x"), (0x452, "STM32F72x/73x"),
    // G0
    (0x456, "STM32G05x/06x"), (0x460, "STM32G07x/08x"), (0x466, "STM32G03x/04x"),
    (0x467, "STM32G0Bx/0Cx"),
    // G4
    (0x468, "STM32G431/441"), (0x469, "STM32G47x/48x"), (0x479, "STM32G491/4A1"),
    // L0
    (0x457, "STM32L01x/02x"), (0x425, "STM32L031/041"), (0x417, "STM32L05x/06x"),
    (0x447, "STM32L07x/08x"),
    // L1
    (0x416, "STM32L1 Cat1/2"), (0x429, "STM32L1 Cat2"), (0x427, "STM32L1 Cat3"),
    (0x436, "STM32L1 Cat4"), (0x437, "STM32L1 Cat5/6"),
    // L4 / L4+
    (0x415, "STM32L4x5/x6"), (0x435, "STM32L43x/44x"), (0x461, "STM32L496/4A6"),
    (0x462, "STM32L45x/46x"), (0x464, "STM32L41x/42x"), (0x470, "STM32L4Rx/4Sx"),
    (0x471, "STM32L4Px/4Qx"),
    // L5
    (0x472, "STM32L5"),
    // H7
    (0x450, "STM32H74x/75x"), (0x480, "STM32H7Ax/7Bx"), (0x483, "STM32H72x/73x"),
    // WB / WL
    (0x494, "STM32WB1x"), (0x495, "STM32WB55"), (0x496, "STM32WB35"), (0x497, "STM32WL5x/Ex"),
    // U5
    (0x482, "STM32U575/585"),
    // C0
    (0x443, "STM32C0"), (0x453, "STM32C0"),
];

/// JEP106 廠商碼 → 廠商名（非 ST/GD32 目標,至少顯示廠商）。查表式。
static VENDOR_NAMES: &[(u16, &str)] = &[
    (dap::JEP_ST, "STMicro"),
    (0x23B, "ARM"),
    (dap::JEP_NORDIC, "Nordic"),
    (dap::JEP_RASPI, "RaspberryPi"),
    (0x015, "NXP"),
    (0x00E, "NXP"),
    (0x017, "TI"),
    (0x01F, "Microchip"),
];

/// CPUID PARTNO → Cortex-M 核心名（通用辨識：任何 ARM Cortex-M 目標的後援顯示）。
static CORE_NAMES: &[(u16, &str)] = &[
    (0xC20, "Cortex-M0"),
    (0xC60, "Cortex-M0+"),
    (0xC21, "Cortex-M1"),
    (0xC23, "Cortex-M3"),
    (0xC24, "Cortex-M4"),
    (0xC27, "Cortex-M7"),
    (0xD20, "Cortex-M23"),
    (0xD21, "Cortex-M33"),
    (0xD22, "Cortex-M55"),
    (0xD23, "Cortex-M85"),
];

/// 在 `(key, name)` 查表中線性搜尋。
fn lookup(table: &[(u16, &'static str)], key: u16) -> Option<&'static str> {
    table.iter().find(|&&(k, _)| k == key).map(|&(_, name)| name)
}

fn core_name(part: u16) -> Option<&'static str> {
    lookup(CORE_NAMES, part)
}

fn chip_name(devid: u16) -> Option<&'static str> {
    lookup(CHIP_NAMES, devid)
}

fn vendor_name(designer: u16) -> Option<&'static str> {
    lookup(VENDOR_NAMES, designer)
}

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

// 走線判定門檻（實機可微調）。ce = SWCLK 邊緣數；dp/ap = link_quality 各 0..16 成功數。
#[cfg(feature = "active-detect")]
const EDGE_MIN: u32 = 4; // ce 視為「SWCLK 有在動」的最小邊緣數
#[cfg(feature = "active-detect")]
const DP_GOOD: u32 = 12; // DP 視為穩定
#[cfg(feature = "active-detect")]
const DP_OK: u32 = 14; // OK 門檻
#[cfg(feature = "active-detect")]
const AP_OK: u32 = 14;

/// 依逐線連通 (dio,clk) + 連線品質 (dp,ap) 判定「哪條線/什麼問題」。
///
/// 重點：**SWCLK 是否連通以 `probe_lines()` 的 `clk` 為準（讀目標內部下拉）**，而非 `ce`。
/// `ce`（擷取窗內 SWCLK 邊緣數）只在 `captured`（本輪真的擷取了波形，即 `used!=0`）時才有意義；
/// 當目標完全沒回應（`used==0`）時 `ce` 會被歸 0，**不可**據此判 SWCLK——那是「no target」，
/// 原因可能是 GND/供電/RDP，與 SWCLK 無關。`ce` 僅在「有擷取卻幾乎無邊緣」時當「探針驅動死」的輔助。
#[cfg(feature = "active-detect")]
fn classify(dio: bool, clk: bool, captured: bool, ce: u32, dp: u32, ap: u32) -> WireVerdict {
    if !dio && !clk {
        return WireVerdict::BothOpen;
    }
    if !clk {
        return WireVerdict::SwclkOpen; // probe_lines：SWCLK 線實體不連通
    }
    if !dio {
        return WireVerdict::SwdioOpen;
    }
    // 兩線都連通 → 改以實際 SWD 交易品質判定。
    if dp == 0 {
        // 完全讀不到 DP：只有「本輪有擷取、卻幾乎無 SWCLK 邊緣」才指向探針 SWCLK 驅動死；
        // 否則據實報 no target（別把 GND/供電/RDP 造成的沉默誤標成 SWCLK BAD）。
        if captured && ce < EDGE_MIN {
            return WireVerdict::SwclkOpen;
        }
        return WireVerdict::NoTarget;
    }
    if dp < DP_GOOD {
        return WireVerdict::GndBad; // DP 成功率低 → 共地阻抗高 / 訊號抖
    }
    if ap == 0 {
        return WireVerdict::PwrParasitic; // DP 穩但 AP 全失敗 → 寄生供電 / RDP1
    }
    if dp >= DP_OK && ap >= AP_OK {
        return WireVerdict::Ok;
    }
    WireVerdict::Unstable
}

/// 自適應 SWCLK（黏著 + 遲滯）：回傳實際採用的速率(kHz)，0 = 沒讀到。
///
/// **重要順序**：先把所有速率的 *single-drop*（純讀 DPIDR、**不寫 TARGETSEL**）掃完，
/// 只有完全沒有單核回應，才最後試 RP multidrop。原因：multidrop 的 TARGETSEL 寫入會把
/// 「現代 DPv2 STM32（F4/F7/G0/G4/L4/H7…）」誤 deselect（寫到不是它的 TARGETID），
/// 之後連 line reset 都救不回 → 整顆 STM32 啞掉。故可偵測的單核目標一律走 single-drop。
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
