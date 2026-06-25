//! SWD 數位邏輯擷取 — 用 **PIO0 的 SM1** 與 DMA 高速取樣 SWCLK/SWDIO，
//! 在 SM0 驅動 SWD 時同步抓波形，供 OLED 畫 2 通道方波（示波器式）。
//!
//! 類比波形 RP2040 在這兩支腳上做不到（ADC 太慢、不在該腳）；此處為數位（1-bit）取樣。

#![allow(dead_code)]

use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::dma::{Channel, Transfer};
use embassy_rp::pac;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{Common, Config, LoadedProgram, ShiftConfig, ShiftDirection, StateMachine};
use fixed::FixedU32;
use fixed::types::extra::U8;

/// 擷取緩衝字數（每字 16 取樣，各 2 bit）。16 字 = 256 取樣。
pub const CAP_WORDS: usize = 16;
/// 總取樣數。
pub const SAMPLES: usize = CAP_WORDS * 16;
/// PIO 取樣分頻（取樣率 = clk_sys / DIVIDER）。
pub const DIVIDER: u32 = 8;

/// 每個取樣（= 顯示每欄）的時間，奈秒。供 OLED 顯示刻度。
pub fn sample_ns() -> u32 {
    (DIVIDER as u64 * 1_000_000_000 / clk_sys_freq() as u64) as u32
}

pub struct LogicCapture<'d> {
    sm: StateMachine<'d, PIO0, 1>,
    dma: Channel<'d>,
    _prog: LoadedProgram<'d, PIO0>,
}

impl<'d> LogicCapture<'d> {
    /// `in_base` = SWCLK 的 GPIO 編號（SWDIO 須為其 +1，相鄰）。
    pub fn new(
        common: &mut Common<'d, PIO0>,
        mut sm: StateMachine<'d, PIO0, 1>,
        dma: Channel<'d>,
        in_base: u8,
    ) -> Self {
        // 自由取樣：每 PIO 週期取一次 SWCLK(bit0)+SWDIO(bit1)，autopush 每 32 bit（16 取樣）一字。
        let prg = pio::pio_asm!(".wrap_target", "    in pins, 2", ".wrap",);
        let loaded = common.load_program(&prg.program);

        let mut cfg = Config::default();
        cfg.use_program(&loaded, &[]);
        cfg.clock_divider = FixedU32::<U8>::from_num(DIVIDER); // clk_sys/DIVIDER ≈ 15.6 MSa/s
        cfg.shift_in = ShiftConfig {
            threshold: 32,
            direction: ShiftDirection::Right, // 先到的取樣在低位
            auto_fill: true,
        };
        sm.set_config(&cfg);

        // in_base = SWCLK（經 PAC 設定，不奪 funcsel；腳已由 SWD make_pio_pin 建立）。
        pac::PIO0.sm(1).pinctrl().modify(|w| w.set_in_base(in_base));

        Self {
            sm,
            dma,
            _prog: loaded,
        }
    }

    /// 重置並啟用 SM（清 FIFO，開始取樣）。
    pub fn start(&mut self) {
        self.sm.set_enable(false);
        self.sm.clear_fifos();
        self.sm.restart();
        self.sm.set_enable(true);
    }

    /// 啟動 DMA 把 RX FIFO 搬進 `buf`（硬體並發）；回傳的 Transfer await 即等填滿。
    pub fn dma_into<'a>(&'a mut self, buf: &'a mut [u32]) -> Transfer<'a> {
        self.sm.rx().dma_pull(&mut self.dma, buf, false)
    }

    pub fn stop(&mut self) {
        self.sm.set_enable(false);
    }
}

/// 把擷取到的字陣列解碼成第 `col` 欄(0..SAMPLES)的 (clk, dio) bit。
#[inline]
pub fn sample_at(buf: &[u32], idx: usize) -> (bool, bool) {
    let w = buf[idx / 16];
    let s = (w >> (2 * (idx % 16))) & 0b11;
    (s & 1 != 0, s & 2 != 0)
}

/// 數擷取窗內 SWCLK/SWDIO 的邊緣(跳變)數與高電位取樣數。回 (clk_e, dio_e, clk_hi, dio_hi)。
pub(crate) fn count_signal(buf: &[u32]) -> (u32, u32, u32, u32) {
    let (mut pc, mut pd) = sample_at(buf, 0);
    let (mut ce, mut de) = (0u32, 0u32);
    let (mut ch, mut dh) = (pc as u32, pd as u32); // 取樣 0 的高電位計入
    for i in 1..SAMPLES {
        let (c, d) = sample_at(buf, i);
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
