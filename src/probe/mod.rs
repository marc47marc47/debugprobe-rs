//! SWD 物理層 — 對應 C 版 `probe.c` + `probe.pio` / `probe_oen.pio`。
//!
//! 把 SWD 的位元序列翻譯成 PIO 命令丟進 PIO0 SM0；SWCLK 週期固定為 4 個 PIO
//! 執行週期。命令字格式（與 C 一致）：
//!
//! ```text
//! | 13:9 |  8  |  7:0  |
//! | Cmd  | Dir | Count |
//! ```
//!
//! Count = 位元數 - 1；Dir = SWDIO output-enable；Cmd = 目標常式的絕對位址
//! （origin + public label 偏移）。
//!
//! 與 C 不同處：PIO 程式重排成讓 `get_next_cmd` 位於 origin，因為 embassy-rp 的
//! `set_config` 會自動 jmp 到 origin 啟動。寫入路徑結尾改用顯式 `jmp get_next_cmd`
//! 取代 C 靠 `.wrap_target` 落空的行為，語意等價。
//!

use embassy_rp::Peri;
use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::gpio::{Drive, Flex, Level, Pull, SlewRate};
use embassy_rp::peripherals::PIO0;
use embassy_time::Timer;
use embassy_rp::pio::{
    Common, Config, Direction, LoadedProgram, Pin, ShiftConfig, ShiftDirection, StateMachine,
};
use fixed::FixedU32;
use fixed::types::extra::U8;

/// PIO 命令種類（對應 C 的 `probe_pio_command_t`）。
#[derive(Clone, Copy)]
enum Cmd {
    Write,
    Turnaround,
    Read,
}

/// public label 的絕對位址（origin + 相對偏移）。
struct CmdAddrs {
    write: u32,
    turnaround: u32,
    read: u32,
}

pub struct Probe<'d> {
    sm: StateMachine<'d, PIO0, 0>,
    addrs: CmdAddrs,
    sys_hz: u32,
    cached_freq_khz: u32,
    // 保留所有權，避免 PIO pin 還原功能或程式被釋放。
    _common: Common<'d, PIO0>,
    _swclk: Pin<'d, PIO0>,
    _swdio: Pin<'d, PIO0>,
    _swdi: Option<Pin<'d, PIO0>>,
    _prog: LoadedProgram<'d, PIO0>,
    reset: Option<Flex<'d>>,
}

impl<'d> Probe<'d> {
    /// 建立並啟動 SWD PIO。`swdi` 為 None 時走 RAW 模式（in-pin = SWDIO）；
    /// 為 Some 時走 SWDI 模式（in-pin = 獨立讀回腳，給 level shifter）。
    pub fn new(
        mut common: Common<'d, PIO0>,
        mut sm: StateMachine<'d, PIO0, 0>,
        mut swclk: Pin<'d, PIO0>,
        mut swdio: Pin<'d, PIO0>,
        mut swdi: Option<Pin<'d, PIO0>>,
        reset_pin: Option<Peri<'d, embassy_rp::peripherals::PIN_1>>,
    ) -> Self {
        // --- 組譯 PIO 程式（get_next_cmd 置於 origin）---
        let prg = pio::pio_asm!(
            ".side_set 1 opt",
            ".wrap_target",
            "public get_next_cmd:",
            "    pull                     side 0x0", // SWCLK 起始為低
            "    out x, 8",                          // 取位元數
            "    out pindirs, 1",                    // 設定 SWDIO 方向
            "    out pc, 5",                         // 跳到命令常式
            "public write_cmd:",
            "public turnaround_cmd:",
            "    pull",
            "write_bitloop:",
            "    out pins, 1          [1] side 0x0", // host 在負緣輸出
            "    jmp x-- write_bitloop [1] side 0x1", // target 在正緣取樣
            "    jmp get_next_cmd",                  // 寫完返回（取代 C 的落空）
            "read_bitloop:",
            "    nop",                               // 取分支時的額外延遲
            "public read_cmd:",
            "    in pins, 1          [1] side 0x1",  // host 在正緣取樣
            "    jmp x-- read_bitloop     side 0x0",
            "    push",
            ".wrap",
        );

        // SWDIO 需有 pull-up，idle 為高。
        swdio.set_pull(Pull::Up);

        // 長線杜邦訊號完整性：drive 與 slew 是獨立旋鈕，兩者兼顧——
        //   高電流(8mA)：撐住長線/電容負載下的邏輯準位（2mA 太弱 → 連 DP 都建不起來）。
        //   慢 slew    ：軟化邊緣、抑制長線反射/振鈴（Fast 邊緣 → DP 過但密集 AP 序列位元錯誤）。
        // 即「強而不陡」：訊號夠強又不振鈴，DP 與 AP 都穩。
        // 輸入端開 Schmitt 觸發抗雜訊，改善 SWDIO/SWDI 讀回穩定度。
        swclk.set_drive_strength(Drive::_8mA);
        swclk.set_slew_rate(SlewRate::Slow);
        swdio.set_drive_strength(Drive::_8mA);
        swdio.set_slew_rate(SlewRate::Slow);
        swdio.set_schmitt(true);
        if let Some(di) = &mut swdi {
            di.set_schmitt(true);
        }

        let loaded = common.load_program(&prg.program);
        let origin = loaded.origin as u32;
        let d = &prg.public_defines;
        let addrs = CmdAddrs {
            write: origin + d.write_cmd as u32,
            turnaround: origin + d.turnaround_cmd as u32,
            read: origin + d.read_cmd as u32,
        };

        let sys_hz = clk_sys_freq();
        let initial_div = Self::divider_for(sys_hz, 1000);

        let mut cfg = Config::default();
        cfg.use_program(&loaded, &[&swclk]); // SWCLK 為 sideset
        cfg.set_out_pins(&[&swdio]);
        cfg.set_set_pins(&[&swdio]);
        match &swdi {
            Some(di) => cfg.set_in_pins(&[di]), // SWDI 模式
            None => cfg.set_in_pins(&[&swdio]), // RAW 模式
        }
        // SWD 為 LSB first：輸入輸出皆右移、不自動填充。
        cfg.shift_out = ShiftConfig {
            threshold: 32,
            direction: ShiftDirection::Right,
            auto_fill: false,
        };
        cfg.shift_in = ShiftConfig {
            threshold: 32,
            direction: ShiftDirection::Right,
            auto_fill: false,
        };
        cfg.clock_divider = FixedU32::<U8>::from_num(initial_div);
        sm.set_config(&cfg);

        // SWCLK + SWDIO 起始為輸出。
        sm.set_pin_dirs(Direction::Out, &[&swclk, &swdio]);
        sm.set_enable(true);

        // 目標 reset 腳：pull-up + 輸入（open-drain 模擬，de-assert）。
        let reset = reset_pin.map(|p| {
            let mut f = Flex::new(p);
            f.set_pull(Pull::Up);
            f.set_as_input();
            f
        });

        Self {
            sm,
            addrs,
            sys_hz,
            cached_freq_khz: 1000,
            _common: common,
            _swclk: swclk,
            _swdio: swdio,
            _swdi: swdi,
            _prog: loaded,
            reset,
        }
    }

    fn divider_for(sys_hz: u32, freq_khz: u32) -> u32 {
        let sys_khz = sys_hz / 1000;
        // 向上取整（否則高速 SWCLK 會更快）。
        let mut d = sys_khz.div_ceil(freq_khz).div_ceil(4);
        if d == 0 {
            d = 1;
        }
        if d > 65535 {
            d = 65535;
        }
        d
    }

    /// 設定 SWCLK 頻率（kHz）。對應 C `probe_set_swclk_freq`。
    pub fn set_swclk_freq(&mut self, freq_khz: u32) {
        let d = Self::divider_for(self.sys_hz, freq_khz);
        self.sm.set_clock_divider(FixedU32::<U8>::from_num(d));
        self.cached_freq_khz = freq_khz;
    }

    /// 目前 SWCLK 設定頻率（kHz）。
    pub fn freq_khz(&self) -> u32 {
        self.cached_freq_khz
    }

    fn fmt_cmd(&self, bit_count: u32, out_en: bool, cmd: Cmd) -> u32 {
        let addr = match cmd {
            Cmd::Write => self.addrs.write,
            Cmd::Turnaround => self.addrs.turnaround,
            Cmd::Read => self.addrs.read,
        };
        ((bit_count.wrapping_sub(1)) & 0xff) | ((out_en as u32) << 8) | (addr << 9)
    }

    /// 寫出 `bit_count` 位元（1..=256）。對應 C `probe_write_bits`。
    pub async fn write_bits(&mut self, bit_count: u32, data: u32) {
        let cmd = self.fmt_cmd(bit_count, true, Cmd::Write);
        self.sm.tx().wait_push(cmd).await;
        self.sm.tx().wait_push(data).await;
    }

    /// 驅動 N 個 hi-z 時脈（turnaround）。對應 C `probe_hiz_clocks`。
    pub async fn hiz_clocks(&mut self, bit_count: u32) {
        let cmd = self.fmt_cmd(bit_count, false, Cmd::Turnaround);
        self.sm.tx().wait_push(cmd).await;
        self.sm.tx().wait_push(0).await;
    }

    /// 讀入 `bit_count` 位元（1..=32）。對應 C `probe_read_bits`。
    pub async fn read_bits(&mut self, bit_count: u32) -> u32 {
        let cmd = self.fmt_cmd(bit_count, false, Cmd::Read);
        self.sm.tx().wait_push(cmd).await;
        let data = self.sm.rx().wait_pull().await;
        if bit_count < 32 {
            data >> (32 - bit_count)
        } else {
            data
        }
    }


    /// 設定/解除 target nRESET（open-drain 模擬）。對應 C `probe_assert_reset`。
    /// `state == false` 代表 assert（驅動為低）；`true` 代表 de-assert（hi-z）。
    pub fn assert_reset(&mut self, state: bool) {
        if let Some(r) = &mut self.reset {
            if !state {
                r.set_as_output();
                r.set_low();
            } else {
                r.set_as_input();
            }
        }
    }

    /// 讀取 nRESET 目前電位。對應 C `probe_reset_level`。
    pub fn reset_level(&self) -> u8 {
        match &self.reset {
            Some(r) => (r.get_level() == Level::High) as u8,
            None => 0,
        }
    }

    /// 偵測 SWDIO / SWCLK 兩條線是否實體連到「有電有地」的目標。
    /// 方法：drive 反向 → 釋放(輸入,無 pull) → 等目標內部 pull 翻轉 → 讀電位。
    /// ARM 目標 SWDIO 內部上拉、SWCLK 內部下拉；連通則被翻向 pull 側、未接則維持驅動值。
    /// 回 (dio_connected, clk_connected)。僅在 host 閒置時呼叫；之後的 swd_health 會重置腳位。
    pub async fn probe_lines(&mut self) -> (bool, bool) {
        // 暫停 SM：否則 SWCLK 為 sideset 腳，SM idle 的 `pull side 0` 會持續把它驅動低，
        // 導致釋放後永遠讀到 LOW（永遠誤判連通）。停 SM 後 sideset 不再驅動。
        self.sm.set_enable(false);

        // --- SWDIO：drive LOW → 釋放 → 連通(上拉)→HIGH ---
        self.sm.set_pin_dirs(Direction::Out, &[&self._swdio]);
        self.sm.set_pins(Level::Low, &[&self._swdio]);
        Timer::after_micros(20).await;
        self._swdio.set_pull(Pull::None);
        self.sm.set_pin_dirs(Direction::In, &[&self._swdio]);
        Timer::after_micros(150).await;
        let dio = (embassy_rp::pac::SIO.gpio_in(0).read() >> crate::board::CONFIG.pins.swdio) & 1 != 0;
        self._swdio.set_pull(Pull::Up); // 還原（idle 高）
        self.sm.set_pin_dirs(Direction::Out, &[&self._swdio]);

        // --- SWCLK：drive HIGH → 釋放 → 連通(下拉)→LOW ---
        self.sm.set_pin_dirs(Direction::Out, &[&self._swclk]);
        self.sm.set_pins(Level::High, &[&self._swclk]);
        Timer::after_micros(20).await;
        self._swclk.set_pull(Pull::None);
        self.sm.set_pin_dirs(Direction::In, &[&self._swclk]);
        Timer::after_micros(150).await;
        let clk = (embassy_rp::pac::SIO.gpio_in(0).read() >> crate::board::CONFIG.pins.swclk) & 1 == 0;
        self.sm.set_pin_dirs(Direction::Out, &[&self._swclk]);
        self.sm.set_pins(Level::Low, &[&self._swclk]); // 還原（idle 低）

        // 重新啟用 SM（PC 維持在 get_next_cmd 等 pull；下次 swd_health 正常運作）。
        self.sm.set_enable(true);
        (dio, clk)
    }

}
