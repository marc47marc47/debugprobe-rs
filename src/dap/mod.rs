//! CMSIS-DAP 核心 — 從零用 Rust 重寫，對應 C 版 `CMSIS_DAP/DAP.c` +
//! `sw_dp_pio.c`（SWD 傳輸層）。僅支援 SWD（JTAG/SWO 停用）。
//!
//! 對外介面 [`Dap::execute_command`]：吃一個 DAP 命令封包、輸出回應、回傳長度。
//! 所有 SWD 時序透過 Phase 2 的 [`Probe`] 以 PIO 完成。

use embassy_time::Timer;

use crate::board;
use crate::probe::Probe;

mod types;
pub use types::*;
mod swd;
mod detect;
pub struct Dap<'d> {
    probe: Probe<'d>,
    serial: &'static str,
    // TransferConfigure
    idle_cycles: u32,
    retry_count: u16,
    // SWD_Configure
    turnaround: u32, // 1..=4
    data_phase: bool,
    // 自主測試學到的「穩定值」(kHz)：AP 也穩的最高 SWCLK；0 = 尚未學到。
    // host 送 DAP_SWJ_Clock 高於此值時會被夾到此值（韌體負責找可行 clock，見 cmd_swj_clock）。
    stable_khz: u32,
    // host 是否下過 DAP_SWJ_Clock。未下過時，host 閒置後預設採 stable_khz（而非開機 ~1MHz）。
    host_clk_set: bool,
}

impl<'d> Dap<'d> {
    pub fn new(probe: Probe<'d>, serial: &'static str) -> Self {
        Self {
            probe,
            serial,
            idle_cycles: 0,
            retry_count: 100,
            turnaround: 1,
            data_phase: false,
            stable_khz: 0,
            host_clk_set: false,
        }
    }

    /// 處理單一 DAP 命令，回傳寫入 `resp` 的位元組數。
    pub async fn execute_command(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let cmd = req[0];
        resp[0] = cmd;
        let Some(c) = DapCmd::from_u8(cmd) else {
            // JTAG / SWO / Vendor / 未知 → invalid
            resp[0] = ID_DAP_INVALID;
            return 1;
        };
        match c {
            DapCmd::Info => self.cmd_info(req, resp),
            DapCmd::HostStatus => self.cmd_host_status(req, resp),
            DapCmd::Connect => self.cmd_connect(req, resp),
            DapCmd::Disconnect => {
                resp[1] = DAP_OK;
                2
            }
            DapCmd::TransferConfigure => self.cmd_transfer_configure(req, resp),
            DapCmd::Transfer => self.cmd_transfer(req, resp).await,
            DapCmd::TransferBlock => self.cmd_transfer_block(req, resp).await,
            DapCmd::TransferAbort => 1, // 無回應
            DapCmd::WriteAbort => self.cmd_write_abort(req, resp).await,
            DapCmd::Delay => self.cmd_delay(req, resp).await,
            DapCmd::ResetTarget => self.cmd_reset_target(resp).await,
            DapCmd::SwjPins => self.cmd_swj_pins(req, resp),
            DapCmd::SwjClock => self.cmd_swj_clock(req, resp),
            DapCmd::SwjSequence => self.cmd_swj_sequence(req, resp).await,
            DapCmd::SwdConfigure => self.cmd_swd_configure(req, resp),
            DapCmd::SwdSequence => self.cmd_swd_sequence(req, resp).await,
        }
    }

    // ---------------- 一般命令 ----------------

    fn cmd_info(&self, req: &[u8], resp: &mut [u8]) -> usize {
        let id = req[1];
        match id {
            INFO_VENDOR => put_str(resp, "Raspberry Pi"),
            INFO_PRODUCT => put_str(resp, board::CONFIG.product),
            INFO_SERIAL => put_str(resp, self.serial),
            INFO_FW_VER => put_str(resp, FW_VERSION),
            INFO_CAPABILITIES => {
                resp[1] = 1;
                resp[2] = CAPABILITIES;
                3
            }
            INFO_PACKET_COUNT => {
                resp[1] = 1;
                resp[2] = DAP_PACKET_COUNT;
                3
            }
            INFO_PACKET_SIZE => {
                resp[1] = 2;
                resp[2..4].copy_from_slice(&DAP_PACKET_SIZE.to_le_bytes());
                4
            }
            _ => {
                resp[1] = 0; // 長度 0
                2
            }
        }
    }

    fn cmd_host_status(&mut self, _req: &[u8], resp: &mut [u8]) -> usize {
        // type/status 用於 LED 指示（Phase 8 接 LED）。
        resp[1] = DAP_OK;
        2
    }

    fn cmd_connect(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let port = req[1];
        // 0 = default(→SWD), 1 = SWD, 2 = JTAG(不支援)
        let selected = if port == 2 { 0 } else { 1 };
        resp[1] = selected;
        2
    }

    fn cmd_transfer_configure(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        self.idle_cycles = req[1] as u32;
        self.retry_count = u16::from_le_bytes([req[2], req[3]]);
        // req[4..6] = match_retry：本韌體不支援 match transfer，忽略不存。
        resp[1] = DAP_OK;
        2
    }

    fn cmd_swd_configure(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let cfg = req[1];
        self.turnaround = (cfg & 0x03) as u32 + 1;
        self.data_phase = (cfg & 0x04) != 0;
        resp[1] = DAP_OK;
        2
    }

    fn cmd_swj_clock(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let hz = u32_le(&req[1..5]);
        let khz = (hz / 1000).max(1);
        self.host_clk_set = true;
        // clamp：host 要求高於已學到的穩定值就忽略、改採穩定值（韌體負責找可行 clock，
        // 避免「host 指定一個達不到的速度」害連線失敗）。尚未學到(0)則照 host。
        // 對協定透明：DAP_SWJ_Clock 只回 OK、不回實際值，且本來整數除頻就讓實際 ≤ 要求。
        let eff = if self.stable_khz > 0 {
            khz.min(self.stable_khz)
        } else {
            khz
        };
        self.probe.set_swclk_freq(eff);
        resp[1] = DAP_OK;
        2
    }

    fn cmd_swj_pins(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let output = req[1];
        let select = req[2];
        // 僅支援 nRESET（bit 7）。
        if select & (1 << 7) != 0 {
            self.probe.assert_reset((output & (1 << 7)) != 0);
        }
        let mut state = 0u8;
        if self.probe.reset_level() != 0 {
            state |= 1 << 7;
        }
        resp[1] = state;
        2
    }

    async fn cmd_delay(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let us = u16::from_le_bytes([req[1], req[2]]);
        Timer::after_micros(us as u64).await;
        resp[1] = DAP_OK;
        2
    }

    async fn cmd_reset_target(&mut self, resp: &mut [u8]) -> usize {
        // 觸發一次 reset 脈衝（若有 reset 腳）。
        self.probe.assert_reset(false);
        Timer::after_millis(2).await;
        self.probe.assert_reset(true);
        resp[1] = DAP_OK;
        resp[2] = 1; // reset sequence 已實作
        3
    }

    async fn cmd_swj_sequence(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let count = if req[1] == 0 { 256 } else { req[1] as u32 };
        let mut n = count;
        let mut di = 2;
        while n > 0 {
            let bits = n.min(8);
            self.probe.write_bits(bits, req[di] as u32).await;
            di += 1;
            n -= bits;
        }
        resp[1] = DAP_OK;
        2
    }

    async fn cmd_swd_sequence(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let seq_count = req[1];
        resp[1] = DAP_OK;
        let mut di = 2;
        let mut ri = 2;
        for _ in 0..seq_count {
            let info = req[di];
            di += 1;
            let mut clk = (info & 0x3f) as u32;
            if clk == 0 {
                clk = 64;
            }
            if info & 0x80 != 0 {
                // 輸入：讀回 clk 位元
                let mut c = clk;
                while c > 0 {
                    let b = c.min(8);
                    resp[ri] = self.probe.read_bits(b).await as u8;
                    ri += 1;
                    c -= b;
                }
            } else {
                // 輸出
                let mut c = clk;
                while c > 0 {
                    let b = c.min(8);
                    self.probe.write_bits(b, req[di] as u32).await;
                    di += 1;
                    c -= b;
                }
            }
        }
        ri
    }

    async fn cmd_write_abort(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let data = u32_le(&req[2..6]);
        // 寫 DP ABORT（addr 0，APnDP=0, RnW=0）。
        self.swd_transfer(DP_ABORT_WR, data).await;
        resp[1] = DAP_OK;
        2
    }


    /// 設定 SWCLK 頻率（kHz）。
    pub fn set_swclk_khz(&mut self, khz: u32) {
        self.probe.set_swclk_freq(khz);
    }
    /// 目前 SWCLK 頻率（kHz）。
    pub fn swclk_khz(&self) -> u32 {
        self.probe.freq_khz()
    }
    /// 自主測試學到「AP 也穩」的最高速率時呼叫，更新穩定值（供 cmd_swj_clock 夾速 + 預設用）。
    pub fn set_stable_khz(&mut self, khz: u32) {
        self.stable_khz = khz;
    }
    /// 自主掃描收尾還原 SWCLK：host 沒下過 clk 且已有穩定值 → 用穩定值當預設；否則還原 `saved`。
    pub fn restore_clk(&mut self, saved: u32) {
        let khz = if !self.host_clk_set && self.stable_khz > 0 {
            self.stable_khz
        } else {
            saved
        };
        self.probe.set_swclk_freq(khz);
    }
    /// 逐線連通偵測（轉呼叫 PIO 物理層 `Probe::probe_lines`）。回 (dio_connected, clk_connected)。
    /// 供走線監測判斷哪條線斷：drive 反向→釋放→讀目標內部 pull 翻轉。僅在 host 閒置時呼叫。
    pub async fn probe_lines(&mut self) -> (bool, bool) {
        self.probe.probe_lines().await
    }
}

/// 把 NUL 結尾字串寫入 DAP_Info 回應（resp[1]=長度含 NUL）。
fn put_str(resp: &mut [u8], s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len() + 1; // 含 NUL
    resp[1] = len as u8;
    resp[2..2 + bytes.len()].copy_from_slice(bytes);
    resp[2 + bytes.len()] = 0;
    2 + len
}
