//! AutoBaud — 對應 C 版 `autobaud.c` + `autobaud.pio`。
//!
//! 用 **PIO1** 量測 UART RX(GP5) 的邊緣間隔來推算 baud rate。關鍵：透過 PAC 把
//! SM 的 in_base/jmp_pin 設成 GP5，但**不呼叫 make_pio_pin**（不更動 funcsel），
//! 因此 UART1 仍擁有 GP5、PIO1 只是讀其輸入同步器（RP2040 的 GPIO 輸入對所有
//! 周邊永遠可見）。主機把 CDC baud 設為魔術值 9728 即觸發偵測。
//!
//! 偵測完成後回傳估算 baud；與 C 的雙 DMA + 雜湊表相比，這裡用軟體讀 FIFO +
//! 「最短且重複出現的間隔 = 1 bit time」估算，較精簡但足以偵測常見 baud。

#![allow(dead_code)]

use embassy_futures::select::select;
use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::pac;
use embassy_rp::peripherals::PIO1;
use embassy_rp::pio::{Common, Config, LoadedProgram, StateMachine};
use embassy_time::{Duration, Timer};
use fixed::FixedU32;
use fixed::types::extra::U8;

/// 觸發 AutoBaud 的魔術 baud（對應 C 的 MAGIC_BAUD 0x2600）。
pub const MAGIC_BAUD: u32 = 9728;

// AutoBaud 調校參數（集中散落的魔術數，實機可微調）。
const SAMPLE_CAP: usize = 512; // 邊緣間隔樣本上限
const COLLECT_MS: u64 = 400; // 收集時間（ms）
const TOL_DIV: u32 = 20; // 容差 = s/TOL_DIV（±5%）
const MIN_REPEAT: usize = 3; // 視為「1 bit time」需重複出現的最少次數
const MIN_SAMPLES: usize = 8; // 樣本太少不估

pub struct AutoBaud<'d> {
    sm: StateMachine<'d, PIO1, 0>,
    _common: Common<'d, PIO1>,
    _prog: LoadedProgram<'d, PIO1>,
    pio_clk: u32,
}

impl<'d> AutoBaud<'d> {
    pub fn new(
        mut common: Common<'d, PIO1>,
        mut sm: StateMachine<'d, PIO1, 0>,
        rx_pin: u8,
    ) -> Self {
        let prg = pio::pio_asm!(
            ".wrap_target",
            "falling:",
            "    wait 0 pin 0",     // 等下降緣
            "    set x, 0",
            "    mov x, ~x",        // x = 0xFFFFFFFF
            "count:",
            "    jmp pin rising",   // 線拉高 → 記錄
            "    jmp x-- count",    // 否則遞減續數
            "rising:",
            "    mov isr, x",
            "    push noblock",
            "    jmp falling",
            ".wrap",
        );
        let loaded = common.load_program(&prg.program);

        let mut cfg = Config::default();
        cfg.use_program(&loaded, &[]);
        cfg.clock_divider = FixedU32::<U8>::from_num(1); // PIO 跑 clk_sys
        sm.set_config(&cfg);

        // 透過 PAC 設 in_base / jmp_pin = GP5（不奪 funcsel）。
        pac::PIO1.sm(0).pinctrl().modify(|w| w.set_in_base(rx_pin));
        pac::PIO1.sm(0).execctrl().modify(|w| w.set_jmp_pin(rx_pin));

        Self {
            sm,
            _common: common,
            _prog: loaded,
            pio_clk: clk_sys_freq(),
        }
    }

    /// 啟動 SM 收集約 400ms 的邊緣間隔，估算 baud 後回傳。
    pub async fn detect(&mut self) -> Option<u32> {
        self.sm.restart();
        self.sm.set_enable(true);
        while self.sm.rx().try_pull().is_some() {} // 清舊資料

        let mut samples: heapless::Vec<u32, SAMPLE_CAP> = heapless::Vec::new();
        let collect = async {
            loop {
                let raw = self.sm.rx().wait_pull().await;
                let cycles = (u32::MAX - raw).saturating_mul(2);
                if cycles > 1 {
                    let _ = samples.push(cycles);
                }
                if samples.is_full() {
                    break;
                }
            }
        };
        let _ = select(Timer::after(Duration::from_millis(COLLECT_MS)), collect).await;
        self.sm.set_enable(false);

        estimate(&samples, self.pio_clk)
    }
}

/// 最短且重複出現（±5%）至少 3 次的間隔視為 1 bit time，換算 baud。
fn estimate(samples: &[u32], pio_clk: u32) -> Option<u32> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let mut best: Option<u32> = None;
    for &s in samples {
        let tol = (s / TOL_DIV) as i64; // ±5%
        let count = samples
            .iter()
            .filter(|&&x| (x as i64 - s as i64).abs() <= tol)
            .count();
        if count >= MIN_REPEAT {
            best = Some(best.map_or(s, |b| b.min(s)));
        }
    }
    let min = best?;
    if min == 0 {
        return None;
    }
    Some(pio_clk / min)
}
