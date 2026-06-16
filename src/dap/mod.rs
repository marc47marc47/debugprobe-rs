//! CMSIS-DAP 核心 — 從零用 Rust 重寫，對應 C 版 `CMSIS_DAP/DAP.c` +
//! `sw_dp_pio.c`（SWD 傳輸層）。僅支援 SWD（JTAG/SWO 停用）。
//!
//! 對外介面 [`Dap::execute_command`]：吃一個 DAP 命令封包、輸出回應、回傳長度。
//! 所有 SWD 時序透過 Phase 2 的 [`Probe`] 以 PIO 完成。

use embassy_time::Timer;

use crate::board;
use crate::probe::Probe;

// --- 能力 / 設定常數（對應 DAP_config.h）---
const DAP_PACKET_SIZE: u16 = 64;
const DAP_PACKET_COUNT: u8 = 8;
const CAPABILITIES: u8 = 0x01; // bit0 = SWD, bit1 = JTAG(0)
const FW_VERSION: &str = "2.1.0";

// --- 回應狀態 ---
const DAP_OK: u8 = 0x00;
const ID_DAP_INVALID: u8 = 0xFF;

// --- DAP 命令 ID ---
const ID_DAP_INFO: u8 = 0x00;
const ID_DAP_HOST_STATUS: u8 = 0x01;
const ID_DAP_CONNECT: u8 = 0x02;
const ID_DAP_DISCONNECT: u8 = 0x03;
const ID_DAP_TRANSFER_CONFIGURE: u8 = 0x04;
const ID_DAP_TRANSFER: u8 = 0x05;
const ID_DAP_TRANSFER_BLOCK: u8 = 0x06;
const ID_DAP_TRANSFER_ABORT: u8 = 0x07;
const ID_DAP_WRITE_ABORT: u8 = 0x08;
const ID_DAP_DELAY: u8 = 0x09;
const ID_DAP_RESET_TARGET: u8 = 0x0A;
const ID_DAP_SWJ_PINS: u8 = 0x10;
const ID_DAP_SWJ_CLOCK: u8 = 0x11;
const ID_DAP_SWJ_SEQUENCE: u8 = 0x12;
const ID_DAP_SWD_CONFIGURE: u8 = 0x13;
const ID_DAP_SWD_SEQUENCE: u8 = 0x1D;

// --- DAP_Info sub-id ---
const INFO_VENDOR: u8 = 0x01;
const INFO_PRODUCT: u8 = 0x02;
const INFO_SERIAL: u8 = 0x03;
const INFO_FW_VER: u8 = 0x04;
const INFO_CAPABILITIES: u8 = 0xF0;
const INFO_PACKET_COUNT: u8 = 0xFE;
const INFO_PACKET_SIZE: u8 = 0xFF;

// --- SWD transfer request bits ---
const REQ_APND_P: u8 = 1 << 0;
const REQ_RNW: u8 = 1 << 1;

// --- SWD ACK ---
const ACK_OK: u8 = 1;
const ACK_WAIT: u8 = 2;
const ACK_FAULT: u8 = 4;
const ACK_ERROR: u8 = 8; // parity / protocol（本地定義）

/// DP RDBUFF 讀取請求（APnDP=0, RnW=1, A[3:2]=11）。
const DP_RDBUFF_READ: u8 = REQ_RNW | (1 << 2) | (1 << 3);

/// DAP 命令 ID → 名稱（供 OLED 活動顯示）。
pub fn cmd_name(id: u8) -> &'static str {
    match id {
        ID_DAP_INFO => "Info",
        ID_DAP_HOST_STATUS => "HostStatus",
        ID_DAP_CONNECT => "Connect",
        ID_DAP_DISCONNECT => "Disconnect",
        ID_DAP_TRANSFER_CONFIGURE => "TransferCfg",
        ID_DAP_TRANSFER => "Transfer",
        ID_DAP_TRANSFER_BLOCK => "TransferBlk",
        ID_DAP_TRANSFER_ABORT => "TransferAbrt",
        ID_DAP_WRITE_ABORT => "WriteABORT",
        ID_DAP_DELAY => "Delay",
        ID_DAP_RESET_TARGET => "ResetTarget",
        ID_DAP_SWJ_PINS => "SWJ_Pins",
        ID_DAP_SWJ_CLOCK => "SWJ_Clock",
        ID_DAP_SWJ_SEQUENCE => "SWJ_Seq",
        ID_DAP_SWD_CONFIGURE => "SWD_Cfg",
        ID_DAP_SWD_SEQUENCE => "SWD_Seq",
        _ => "?",
    }
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn put_u32_le(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_le_bytes());
}

pub struct Dap<'d> {
    probe: Probe<'d>,
    serial: &'static str,
    // TransferConfigure
    idle_cycles: u32,
    retry_count: u16,
    #[allow(dead_code)]
    match_retry: u16,
    // SWD_Configure
    turnaround: u32, // 1..=4
    data_phase: bool,
}

impl<'d> Dap<'d> {
    pub fn new(probe: Probe<'d>, serial: &'static str) -> Self {
        Self {
            probe,
            serial,
            idle_cycles: 0,
            retry_count: 100,
            match_retry: 0,
            turnaround: 1,
            data_phase: false,
        }
    }

    /// 處理單一 DAP 命令，回傳寫入 `resp` 的位元組數。
    pub async fn execute_command(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let cmd = req[0];
        resp[0] = cmd;
        match cmd {
            ID_DAP_INFO => self.cmd_info(req, resp),
            ID_DAP_HOST_STATUS => self.cmd_host_status(req, resp),
            ID_DAP_CONNECT => self.cmd_connect(req, resp),
            ID_DAP_DISCONNECT => {
                resp[1] = DAP_OK;
                2
            }
            ID_DAP_TRANSFER_CONFIGURE => self.cmd_transfer_configure(req, resp),
            ID_DAP_TRANSFER => self.cmd_transfer(req, resp).await,
            ID_DAP_TRANSFER_BLOCK => self.cmd_transfer_block(req, resp).await,
            ID_DAP_TRANSFER_ABORT => 1, // 無回應
            ID_DAP_WRITE_ABORT => self.cmd_write_abort(req, resp).await,
            ID_DAP_DELAY => self.cmd_delay(req, resp).await,
            ID_DAP_RESET_TARGET => self.cmd_reset_target(resp).await,
            ID_DAP_SWJ_PINS => self.cmd_swj_pins(req, resp),
            ID_DAP_SWJ_CLOCK => self.cmd_swj_clock(req, resp),
            ID_DAP_SWJ_SEQUENCE => self.cmd_swj_sequence(req, resp).await,
            ID_DAP_SWD_CONFIGURE => self.cmd_swd_configure(req, resp),
            ID_DAP_SWD_SEQUENCE => self.cmd_swd_sequence(req, resp).await,
            _ => {
                // JTAG / SWO / Vendor / 未知 → invalid
                resp[0] = ID_DAP_INVALID;
                1
            }
        }
    }

    // ---------------- 一般命令 ----------------

    fn cmd_info(&self, req: &[u8], resp: &mut [u8]) -> usize {
        let id = req[1];
        match id {
            INFO_VENDOR => put_str(resp, "Raspberry Pi"),
            INFO_PRODUCT => put_str(resp, board::PRODUCT_STRING),
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
        self.match_retry = u16::from_le_bytes([req[4], req[5]]);
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
        self.probe.set_swclk_freq(khz);
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
        self.swd_transfer(0x00, data).await;
        resp[1] = DAP_OK;
        2
    }

    // ---------------- Transfer / TransferBlock ----------------

    /// DAP_Transfer：處理多筆任意傳輸（含 posted AP read）。
    async fn cmd_transfer(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let _dap_index = req[1];
        let count = req[2];
        let mut qi = 3; // 請求游標
        let mut ri = 3; // 回應資料游標（resp[1]=count, resp[2]=ack）
        let mut processed = 0u8;
        let mut ack = ACK_OK;
        let mut post_read = false; // 是否有尚未取回的 posted AP read

        'outer: for _ in 0..count {
            let request = req[qi];
            qi += 1;

            if request & REQ_RNW != 0 {
                // 讀取
                if request & REQ_APND_P != 0 {
                    // AP read：posted
                    if post_read {
                        // 取回前一筆 posted 結果並再 post 這次
                        let (a, val) = self.transfer_retry(request, 0).await;
                        ack = a;
                        if a != ACK_OK {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                    } else {
                        let (a, _) = self.transfer_retry(request, 0).await;
                        ack = a;
                        if a != ACK_OK {
                            break 'outer;
                        }
                        post_read = true;
                    }
                } else {
                    // DP read：立即
                    if post_read {
                        // 先用 RDBUFF 取回 posted AP read
                        let (a, val) = self.transfer_retry(DP_RDBUFF_READ, 0).await;
                        ack = a;
                        if a != ACK_OK {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                        post_read = false;
                    }
                    let (a, val) = self.transfer_retry(request, 0).await;
                    ack = a;
                    if a != ACK_OK {
                        break 'outer;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                }
            } else {
                // 寫入
                if post_read {
                    let (a, val) = self.transfer_retry(DP_RDBUFF_READ, 0).await;
                    ack = a;
                    if a != ACK_OK {
                        break 'outer;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                    post_read = false;
                }
                let data = u32_le(&req[qi..qi + 4]);
                qi += 4;
                let (a, _) = self.transfer_retry(request, data).await;
                ack = a;
                if a != ACK_OK {
                    break 'outer;
                }
            }
            processed += 1;
        }

        // 收尾：取回仍 pending 的 posted read
        if post_read && ack == ACK_OK {
            let (a, val) = self.transfer_retry(DP_RDBUFF_READ, 0).await;
            ack = a;
            if a == ACK_OK {
                put_u32_le(&mut resp[ri..], val);
                ri += 4;
            }
        }

        resp[1] = processed;
        resp[2] = ack;
        ri
    }

    /// DAP_TransferBlock：對同一暫存器連續多筆讀或寫。
    async fn cmd_transfer_block(&mut self, req: &[u8], resp: &mut [u8]) -> usize {
        let _dap_index = req[1];
        let count = u16::from_le_bytes([req[2], req[3]]) as u32;
        let request = req[4];
        let mut qi = 5;
        let mut ri = 4; // resp[1..3]=count, resp[3]=ack
        let mut processed: u16 = 0;
        let mut ack = ACK_OK;

        if request & REQ_RNW != 0 {
            // 讀：AP read 需先 post 一次再連續取回
            if request & REQ_APND_P != 0 {
                let (a, _) = self.transfer_retry(request, 0).await;
                ack = a;
                if a == ACK_OK {
                    for i in 0..count {
                        let req_i = if i == count - 1 { DP_RDBUFF_READ } else { request };
                        let (a, val) = self.transfer_retry(req_i, 0).await;
                        ack = a;
                        if a != ACK_OK {
                            break;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                        processed += 1;
                    }
                }
            } else {
                for _ in 0..count {
                    let (a, val) = self.transfer_retry(request, 0).await;
                    ack = a;
                    if a != ACK_OK {
                        break;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                    processed += 1;
                }
            }
        } else {
            // 寫
            for _ in 0..count {
                let data = u32_le(&req[qi..qi + 4]);
                qi += 4;
                let (a, _) = self.transfer_retry(request, data).await;
                ack = a;
                if a != ACK_OK {
                    break;
                }
                processed += 1;
            }
        }

        resp[1..3].copy_from_slice(&processed.to_le_bytes());
        resp[3] = ack;
        ri
    }

    /// 帶 WAIT 重試的單筆 SWD transfer。
    async fn transfer_retry(&mut self, request: u8, wdata: u32) -> (u8, u32) {
        let mut tries = self.retry_count as i32;
        loop {
            let (ack, val) = self.swd_transfer(request, wdata).await;
            if ack != ACK_WAIT || tries <= 0 {
                return (ack, val);
            }
            tries -= 1;
        }
    }

    // ---------------- 低階 SWD 傳輸（對應 sw_dp_pio.c SWD_Transfer）----------------

    async fn swd_transfer(&mut self, request: u8, wdata: u32) -> (u8, u32) {
        // 組請求封包：start(1) | A[..] | parity | stop(0) | park(1)
        let mut prq: u32 = 1 << 0; // start
        let mut parity = 0u32;
        for n in 0..4 {
            let bit = ((request >> n) & 1) as u32;
            prq |= bit << (n + 1);
            parity += bit;
        }
        prq |= (parity & 1) << 5; // parity
        prq |= 1 << 7; // park
        self.probe.write_bits(8, prq).await;

        // turnaround + 3-bit ACK
        let ackr = self.probe.read_bits(self.turnaround + 3).await;
        let ack = ((ackr >> self.turnaround) & 0x7) as u8;

        if ack == ACK_OK {
            if request & REQ_RNW != 0 {
                // 讀資料相
                let val = self.probe.read_bits(32).await;
                let par = self.probe.read_bits(1).await;
                let mut a = ACK_OK;
                if (val.count_ones() & 1) != (par & 1) {
                    a = ACK_ERROR;
                }
                self.probe.hiz_clocks(self.turnaround).await; // line idle turnaround
                self.idle().await;
                return (a, val);
            } else {
                // 寫資料相
                self.probe.hiz_clocks(self.turnaround).await; // write turnaround
                self.probe.write_bits(32, wdata).await;
                self.probe.write_bits(1, wdata.count_ones() & 1).await;
                self.idle().await;
                return (ACK_OK, 0);
            }
        }

        if ack == ACK_WAIT || ack == ACK_FAULT {
            if self.data_phase && (request & REQ_RNW != 0) {
                // dummy read 32+1
                self.clock_in_discard(33).await;
            }
            self.probe.hiz_clocks(self.turnaround).await;
            if self.data_phase && (request & REQ_RNW == 0) {
                self.probe.write_bits(32, 0).await;
                self.probe.write_bits(1, 0).await;
            }
            return (ack, 0);
        }

        // 協定錯誤：退避 turnaround + 32 + 1 位元
        self.clock_in_discard(self.turnaround + 32 + 1).await;
        (ack, 0)
    }

    async fn idle(&mut self) {
        let mut n = self.idle_cycles;
        while n > 0 {
            let c = n.min(256);
            self.probe.write_bits(c, 0).await;
            n -= c;
        }
    }

    async fn clock_in_discard(&mut self, mut n: u32) {
        while n > 0 {
            let c = n.min(32);
            let _ = self.probe.read_bits(c).await;
            n -= c;
        }
    }

    // ---------------- 自主目標偵測（供 OLED 顯示 layer 2 晶片型號）----------------

    /// SWD line reset：>=50 個 SWCLK 週期、SWDIO 持高。
    async fn line_reset(&mut self) {
        self.probe.write_bits(32, 0xFFFF_FFFF).await;
        self.probe.write_bits(32, 0xFFFF_FFFF).await; // 共 64 高，足夠
    }

    /// 經 AHB-AP 讀一個 32-bit 記憶體字（posted read + RDBUFF）。全程 WAIT 重試。
    async fn read_mem32(&mut self, addr: u32) -> Option<u32> {
        // AP CSW = 32-bit word、single（probe-rs/openocd 對 STM32 常用值）
        if self.transfer_retry(0x01, 0x2300_0052).await.0 != ACK_OK {
            return None;
        }
        // AP TAR = addr
        if self.transfer_retry(0x05, addr).await.0 != ACK_OK {
            return None;
        }
        // AP read DRW（posted，回傳前一筆，丟棄；AHB 讀可能 WAIT → 重試）
        if self.transfer_retry(0x0F, 0).await.0 != ACK_OK {
            return None;
        }
        // DP RDBUFF 取實際值
        let (ack, val) = self.transfer_retry(DP_RDBUFF_READ, 0).await;
        if ack != ACK_OK { None } else { Some(val) }
    }

    /// host 閒置時自主用 SWD 連線目標，讀 DBGMCU_IDCODE 取 DEV_ID（12-bit）。
    /// 自包含（含 line reset + JTAG→SWD 切換 + debug powerup + ACK 輪詢）；無目標/失敗回 None。
    /// 注意：會做 SWD line reset，故僅應在 host **未在使用 DAP** 時呼叫。
    pub async fn detect_target_devid(&mut self) -> Option<u16> {
        // line reset → JTAG-to-SWD 切換序列(0xE79E, LSB first) → line reset → idle
        self.line_reset().await;
        self.probe.write_bits(16, 0xE79E).await;
        self.line_reset().await;
        self.probe.write_bits(8, 0).await; // >=2 idle cycles

        // 讀 DPIDR（DP addr0, RnW）；非 OK 代表沒有 SWD 目標。
        if self.transfer_retry(0x02, 0).await.0 != ACK_OK {
            return None;
        }
        let _ = self.transfer_retry(0x00, 0x1E).await; // DP ABORT：清 sticky error
        let _ = self.transfer_retry(0x08, 0).await; // DP SELECT = 0（APSEL0, bank0）
        let _ = self.transfer_retry(0x04, 0x5000_0000).await; // CTRL/STAT：CSYS/CDBG PWRUPREQ

        // 輪詢 powerup ACK（CDBGPWRUPACK bit29 | CSYSPWRUPACK bit31）後才能存取 AP。
        let mut powered = false;
        for _ in 0..20 {
            let (ack, v) = self.transfer_retry(0x06, 0).await; // DP read CTRL/STAT
            if ack == ACK_OK && (v & 0xA000_0000) == 0xA000_0000 {
                powered = true;
                break;
            }
        }
        if !powered {
            return None;
        }

        // DBGMCU_IDCODE @ 0xE0042000（Cortex-M3/M4 STM32 系列）；DEV_ID = bits[11:0]。
        let v = self.read_mem32(0xE004_2000).await?;
        let devid = (v & 0xFFF) as u16;
        if devid == 0 || devid == 0xFFF {
            None
        } else {
            Some(devid)
        }
    }

    /// 輕量「在不在」偵測：只做 SWD 喚醒 + 讀 DPIDR（不碰 powerup/AP/記憶體，微秒級）。
    /// 回 true 代表目標有回應 SWD（用於拔插事件偵測，省去持續完整掃描）。
    pub async fn target_present(&mut self) -> bool {
        self.line_reset().await;
        self.probe.write_bits(16, 0xE79E).await; // JTAG→SWD 切換
        self.line_reset().await;
        self.probe.write_bits(8, 0).await; // idle
        self.transfer_retry(0x02, 0).await.0 == ACK_OK // 讀 DPIDR
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
