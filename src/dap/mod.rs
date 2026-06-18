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

/// DAP 命令 ID（取代 ID_DAP_* 常數）。值 = CMSIS-DAP 命令位元組；未列出者 = 不支援(JTAG/SWO/Vendor)。
#[derive(Clone, Copy)]
enum DapCmd {
    Info,
    HostStatus,
    Connect,
    Disconnect,
    TransferConfigure,
    Transfer,
    TransferBlock,
    TransferAbort,
    WriteAbort,
    Delay,
    ResetTarget,
    SwjPins,
    SwjClock,
    SwjSequence,
    SwdConfigure,
    SwdSequence,
}

impl DapCmd {
    /// 命令位元組 → DapCmd；不支援的命令回 None（→ DAP_INVALID）。
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x00 => Self::Info,
            0x01 => Self::HostStatus,
            0x02 => Self::Connect,
            0x03 => Self::Disconnect,
            0x04 => Self::TransferConfigure,
            0x05 => Self::Transfer,
            0x06 => Self::TransferBlock,
            0x07 => Self::TransferAbort,
            0x08 => Self::WriteAbort,
            0x09 => Self::Delay,
            0x0A => Self::ResetTarget,
            0x10 => Self::SwjPins,
            0x11 => Self::SwjClock,
            0x12 => Self::SwjSequence,
            0x13 => Self::SwdConfigure,
            0x1D => Self::SwdSequence,
            _ => return None,
        })
    }
    /// 命令名稱（供 OLED 活動顯示）。
    fn name(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::HostStatus => "HostStatus",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::TransferConfigure => "TransferCfg",
            Self::Transfer => "Transfer",
            Self::TransferBlock => "TransferBlk",
            Self::TransferAbort => "TransferAbrt",
            Self::WriteAbort => "WriteABORT",
            Self::Delay => "Delay",
            Self::ResetTarget => "ResetTarget",
            Self::SwjPins => "SWJ_Pins",
            Self::SwjClock => "SWJ_Clock",
            Self::SwjSequence => "SWJ_Seq",
            Self::SwdConfigure => "SWD_Cfg",
            Self::SwdSequence => "SWD_Seq",
        }
    }
}

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

/// SWD 傳輸的 ACK 結果（取代裸 u8）：線上 3-bit ACK + 本地 parity/協定錯誤。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ack {
    Ok,       // 線上 0b001
    Wait,     // 線上 0b010
    Fault,    // 線上 0b100
    Parity,   // Ack::Ok 後讀資料相 parity 錯誤（本地）
    Protocol, // 其他（無回應/協定錯誤）
}

impl Ack {
    /// 線上 3-bit ACK → Ack（1=OK、2=WAIT、4=FAULT、其餘=協定錯誤）。
    fn from_swd(bits: u8) -> Self {
        match bits & 0x7 {
            1 => Ack::Ok,
            2 => Ack::Wait,
            4 => Ack::Fault,
            _ => Ack::Protocol,
        }
    }
    /// 回傳給 host 的 DAP_Transfer 回應 ACK byte（與原本裸值一致：OK=1/WAIT=2/FAULT=4/parity=8/其他=7）。
    fn to_byte(self) -> u8 {
        match self {
            Ack::Ok => 1,
            Ack::Wait => 2,
            Ack::Fault => 4,
            Ack::Parity => 8,
            Ack::Protocol => 7,
        }
    }
}

/// 組 SWD 傳輸 request byte（傳給 `swd_transfer`/`transfer_retry`）：
/// bit0=APnDP、bit1=RnW、bit2=A[2]、bit3=A[3]。`a23` = `(A3<<1)|A2`（= 暫存器位址 / 4）。
const fn swd_req(ap: bool, rnw: bool, a23: u8) -> u8 {
    (ap as u8) | ((rnw as u8) << 1) | ((a23 & 0x3) << 2)
}

// 韌體內部自主存取用的具名 DP/AP 請求（取代散落的裸 request byte 魔術數）。
const DP_DPIDR_RD: u8 = swd_req(false, true, 0b00); // DP 讀 DPIDR（addr 0x0）
const DP_ABORT_WR: u8 = swd_req(false, false, 0b00); // DP 寫 ABORT（addr 0x0）
const DP_CTRLSTAT_WR: u8 = swd_req(false, false, 0b01); // DP 寫 CTRL/STAT（addr 0x4）
const DP_CTRLSTAT_RD: u8 = swd_req(false, true, 0b01); // DP 讀 CTRL/STAT（addr 0x4）
const DP_SELECT_WR: u8 = swd_req(false, false, 0b10); // DP 寫 SELECT（addr 0x8）
const DP_RDBUFF_RD: u8 = swd_req(false, true, 0b11); // DP 讀 RDBUFF（addr 0xC）
const AP_CSW_WR: u8 = swd_req(true, false, 0b00); // AP 寫 CSW（addr 0x0）
const AP_TAR_WR: u8 = swd_req(true, false, 0b01); // AP 寫 TAR（addr 0x4）
const AP_DRW_RD: u8 = swd_req(true, true, 0b11); // AP 讀 DRW（addr 0xC）

/// DAP 命令 ID → 名稱（供 OLED 活動顯示）。
pub fn cmd_name(id: u8) -> &'static str {
    match DapCmd::from_u8(id) {
        Some(c) => c.name(),
        None => "?",
    }
}

/// 讀保護（RDP）等級。跨 task 以 `to_u8`/`from_u8` 經 atomic 傳遞；`label()` 供 OLED 顯示。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RdpLevel {
    Open,    // L0：可燒
    Level1,  // L1：需 unlock（mass erase）
    Level2,  // L2：鎖死，SWD 不可用
    Unknown, // 未知 / 不支援該家族
}

impl RdpLevel {
    pub fn to_u8(self) -> u8 {
        match self {
            RdpLevel::Open => 0,
            RdpLevel::Level1 => 1,
            RdpLevel::Level2 => 2,
            RdpLevel::Unknown => 0xFF,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => RdpLevel::Open,
            1 => RdpLevel::Level1,
            2 => RdpLevel::Level2,
            _ => RdpLevel::Unknown,
        }
    }
    /// OLED「可燒狀態」行文字。
    pub fn label(self) -> &'static str {
        match self {
            RdpLevel::Open => "flash OK (RDP0)",
            RdpLevel::Level1 => "flash LOCK(RDP1)",
            RdpLevel::Level2 => "flash RDP2 dead",
            RdpLevel::Unknown => "flash unknown",
        }
    }
}

/// 自動偵測到的目標資訊（供 OLED 顯示）。
pub struct TargetInfo {
    /// CoreSight ROM table 的 JEP106 廠商碼（cc<<7|id）；0=未知。跨廠牌辨識用。
    pub designer: u16,
    /// ST DBGMCU DEV_ID（僅 ST/GD32 有效，否則 0）。
    pub devid: u16,
    /// 廠商專屬 part（目前用於 Nordic FICR.INFO.PART，如 0x52832）；0=無。
    pub part: u32,
    /// 讀保護等級。
    pub rdp: RdpLevel,
}

/// 常見 JEP106 廠商碼（cc<<7 | 7-bit id）。
pub const JEP_ST: u16 = 0x020; // STMicroelectronics（GD32 亦複製此 ROM table 廠商碼）
pub const JEP_NORDIC: u16 = 0x244; // Nordic Semiconductor
pub const JEP_RASPI: u16 = 0x493; // Raspberry Pi

/// RP2040 multidrop SWD TARGETSEL（core0）：TINSTANCE=0 | TARGETID=0x01002927。
/// （core1 = 0x11002927；RP2350 的 TARGETID 不同，本版先支援 RP2040。）
const TARGETSEL_RP_CORE0: u32 = 0x0100_2927;

/// 讀保護（RDP）所在的 option 暫存器家族。各家族暫存器位址/格式不同。
enum RdpReg {
    /// F2/F4/F7：FLASH_OPTCR @ 0x40023C14，RDP=bits[15:8]（0xAA=L0, 0xCC=L2, 其他=L1）。
    Optcr,
    /// F0/F1/F3/GD32F1：FLASH_OBR @ 0x4002201C，bit1 RDPRT（1=保護=L1, 0=L0）。
    Obr,
    /// L4/G0/G4：FLASH_OPTR @ 0x40022020，RDP=bits[7:0]（0xAA=L0, 0xCC=L2, 其他=L1）。
    Optr,
    /// 其他家族（H7/L0/L1/L5/U5/WB/WL/C0…）暫存器各異 → 不解讀，顯示 unknown。
    Unknown,
}

/// 依 DBGMCU DEV_ID 判斷 RDP 暫存器家族。
fn rdp_reg(devid: u16) -> RdpReg {
    match devid {
        // F2 / F4 / F7
        0x411 | 0x413 | 0x419 | 0x421 | 0x423 | 0x431 | 0x433 | 0x434 | 0x441 | 0x449 | 0x451
        | 0x452 | 0x458 | 0x463 => RdpReg::Optcr,
        // F0 / F1 / F3 / GD32F1
        0x410 | 0x412 | 0x414 | 0x418 | 0x420 | 0x428 | 0x430 | 0x440 | 0x442 | 0x444 | 0x445
        | 0x448 | 0x422 | 0x432 | 0x438 | 0x439 | 0x446 => RdpReg::Obr,
        // L4 / G0 / G4
        0x415 | 0x435 | 0x461 | 0x462 | 0x464 | 0x470 | 0x471 | 0x456 | 0x460 | 0x466 | 0x467
        | 0x468 | 0x469 | 0x479 => RdpReg::Optr,
        _ => RdpReg::Unknown,
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
        self.swd_transfer(DP_ABORT_WR, data).await;
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
        let mut ack = Ack::Ok;
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
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                    } else {
                        let (a, _) = self.transfer_retry(request, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        post_read = true;
                    }
                } else {
                    // DP read：立即
                    if post_read {
                        // 先用 RDBUFF 取回 posted AP read
                        let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
                        ack = a;
                        if a != Ack::Ok {
                            break 'outer;
                        }
                        put_u32_le(&mut resp[ri..], val);
                        ri += 4;
                        post_read = false;
                    }
                    let (a, val) = self.transfer_retry(request, 0).await;
                    ack = a;
                    if a != Ack::Ok {
                        break 'outer;
                    }
                    put_u32_le(&mut resp[ri..], val);
                    ri += 4;
                }
            } else {
                // 寫入
                if post_read {
                    let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
                    ack = a;
                    if a != Ack::Ok {
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
                if a != Ack::Ok {
                    break 'outer;
                }
            }
            processed += 1;
        }

        // 收尾：取回仍 pending 的 posted read
        if post_read && ack == Ack::Ok {
            let (a, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
            ack = a;
            if a == Ack::Ok {
                put_u32_le(&mut resp[ri..], val);
                ri += 4;
            }
        }

        resp[1] = processed;
        resp[2] = ack.to_byte();
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
        let mut ack = Ack::Ok;

        if request & REQ_RNW != 0 {
            // 讀：AP read 需先 post 一次再連續取回
            if request & REQ_APND_P != 0 {
                let (a, _) = self.transfer_retry(request, 0).await;
                ack = a;
                if a == Ack::Ok {
                    for i in 0..count {
                        let req_i = if i == count - 1 { DP_RDBUFF_RD } else { request };
                        let (a, val) = self.transfer_retry(req_i, 0).await;
                        ack = a;
                        if a != Ack::Ok {
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
                    if a != Ack::Ok {
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
                if a != Ack::Ok {
                    break;
                }
                processed += 1;
            }
        }

        resp[1..3].copy_from_slice(&processed.to_le_bytes());
        resp[3] = ack.to_byte();
        ri
    }

    /// 帶 WAIT 重試的單筆 SWD transfer。
    async fn transfer_retry(&mut self, request: u8, wdata: u32) -> (Ack, u32) {
        let mut tries = self.retry_count as i32;
        loop {
            let (ack, val) = self.swd_transfer(request, wdata).await;
            if ack != Ack::Wait || tries <= 0 {
                return (ack, val);
            }
            tries -= 1;
        }
    }

    // ---------------- 低階 SWD 傳輸（對應 sw_dp_pio.c SWD_Transfer）----------------

    async fn swd_transfer(&mut self, request: u8, wdata: u32) -> (Ack, u32) {
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
        let ack = Ack::from_swd(((ackr >> self.turnaround) & 0x7) as u8);

        if ack == Ack::Ok {
            if request & REQ_RNW != 0 {
                // 讀資料相
                let val = self.probe.read_bits(32).await;
                let par = self.probe.read_bits(1).await;
                let mut a = Ack::Ok;
                if (val.count_ones() & 1) != (par & 1) {
                    a = Ack::Parity;
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
                return (Ack::Ok, 0);
            }
        }

        if ack == Ack::Wait || ack == Ack::Fault {
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
        // 最少 8 個 idle clock：host(OpenOCD/probe-rs)常把 idle_cycles 設 0、密集連發 AP 交易，
        // 長線在「讀資料 Hi-Z → 下一筆驅動」之間來不及 settle → AP 間歇失敗（DP 單筆卻沒事）。
        // 強制留 settle 時間，把邊際的密集 AP 序列拉穩。usbipd-rs 因單筆有間隔故原本就穩。
        let mut n = self.idle_cycles.max(8);
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

    /// dormant → SWD 喚醒序列（ADIv5 選擇警示序列）。multidrop DP(RP2040)需要此序列。
    /// 8 高 → 128-bit selection alert(LSB-first) → 4 低 → 8-bit 啟用碼 0x1A。write_bits 為 LSB-first。
    async fn swd_dormant_to_swd(&mut self) {
        self.probe.write_bits(8, 0xFF).await; // >=8 cycles SWDIO 高
        // 128-bit selection alert = 0x19BC0EA2_E3DDAFE9_86852D95_6209F392，LSB-first 位元組序：
        const ALERT: [u8; 16] = [
            0x92, 0xf3, 0x09, 0x62, 0x95, 0x2d, 0x85, 0x86, 0xe9, 0xaf, 0xdd, 0xe3, 0xa2, 0x0e,
            0xbc, 0x19,
        ];
        for b in ALERT {
            self.probe.write_bits(8, b as u32).await;
        }
        self.probe.write_bits(4, 0).await; // 4 cycles 低
        self.probe.write_bits(8, 0x1A).await; // SWD 啟用碼 0x1A
    }

    /// 寫 DP TARGETSEL（addr 0xC）：multidrop 選目標。**target 不驅動 ACK**，故忽略 ACK、照常送資料。
    async fn swd_write_targetsel(&mut self, id: u32) {
        // request: start=1 | APnDP=0 | RnW=0 | A2=1(bit3) | A3=1(bit4) | parity=0 | stop=0 | park=1 = 0x99
        let prq: u32 = 1 | (1 << 3) | (1 << 4) | (1 << 7);
        self.probe.write_bits(8, prq).await;
        let _ = self.probe.read_bits(self.turnaround + 3).await; // trn + ACK（忽略）
        self.probe.hiz_clocks(self.turnaround).await; // trn 回主機
        self.probe.write_bits(32, id).await; // 32-bit TARGETSEL
        self.probe.write_bits(1, id.count_ones() & 1).await; // parity
        self.idle().await;
    }

    /// 嘗試 RP2040/RP2350 multidrop 選核心：dormant→SWD + line reset + TARGETSEL(core0) + 讀 DPIDR。
    /// 成功（讀到 DPIDR）回 true。供單一 DP 無回應時自動改試 multidrop 目標。
    pub async fn swd_select_rp(&mut self) -> bool {
        self.line_reset().await;
        self.swd_dormant_to_swd().await;
        self.line_reset().await;
        self.probe.write_bits(8, 0).await; // >=2 idle
        self.swd_write_targetsel(TARGETSEL_RP_CORE0).await;
        self.swd_read_dpidr().await
    }

    /// 經 AHB-AP 讀一個 32-bit 記憶體字（posted read + RDBUFF）。全程 WAIT 重試。
    async fn read_mem32(&mut self, addr: u32) -> Option<u32> {
        // AP CSW = 32-bit word、single（probe-rs/openocd 對 STM32 常用值）
        if self.transfer_retry(AP_CSW_WR, 0x2300_0052).await.0 != Ack::Ok {
            return None;
        }
        // AP TAR = addr
        if self.transfer_retry(AP_TAR_WR, addr).await.0 != Ack::Ok {
            return None;
        }
        // AP read DRW（posted，回傳前一筆，丟棄；AHB 讀可能 WAIT → 重試）
        if self.transfer_retry(AP_DRW_RD, 0).await.0 != Ack::Ok {
            return None;
        }
        // DP RDBUFF 取實際值
        let (ack, val) = self.transfer_retry(DP_RDBUFF_RD, 0).await;
        if ack != Ack::Ok { None } else { Some(val) }
    }

    /// host 閒置時自主用 SWD 連線目標，讀 DBGMCU_IDCODE 取 DEV_ID（12-bit）。
    /// 自包含（含 line reset + JTAG→SWD 切換 + debug powerup + ACK 輪詢）；無目標/失敗回 None。
    /// 注意：會做 SWD line reset，故僅應在 host **未在使用 DAP** 時呼叫。
    pub async fn detect_target(&mut self) -> Option<TargetInfo> {
        // line reset → JTAG-to-SWD 切換序列(0xE79E, LSB first) → line reset → idle
        self.line_reset().await;
        self.probe.write_bits(16, 0xE79E).await;
        self.line_reset().await;
        self.probe.write_bits(8, 0).await; // >=2 idle cycles

        // 讀 DPIDR（DP addr0, RnW）；單一 DP 無回應 → 試 RP2040/RP2350 multidrop 選核心。
        let mut is_rp = false;
        if self.transfer_retry(DP_DPIDR_RD, 0).await.0 != Ack::Ok {
            if self.swd_select_rp().await {
                is_rp = true;
            } else {
                return None;
            }
        }
        let _ = self.transfer_retry(DP_ABORT_WR, 0x1E).await; // DP ABORT：清 sticky error
        let _ = self.transfer_retry(DP_SELECT_WR, 0).await; // DP SELECT = 0（APSEL0, bank0）
        let _ = self.transfer_retry(DP_CTRLSTAT_WR, 0x5000_0000).await; // CTRL/STAT：CSYS/CDBG PWRUPREQ

        // 輪詢 powerup ACK（CDBGPWRUPACK bit29 | CSYSPWRUPACK bit31）後才能存取 AP。
        let mut powered = false;
        for _ in 0..20 {
            let (ack, v) = self.transfer_retry(DP_CTRLSTAT_RD, 0).await; // DP read CTRL/STAT
            if ack == Ack::Ok && (v & 0xA000_0000) == 0xA000_0000 {
                powered = true;
                break;
            }
        }
        if !powered {
            return None;
        }

        // RP2040/RP2350：無 STM32 DBGMCU，已由 multidrop 選到核心即視為偵測成功，回報 RaspberryPi。
        if is_rp {
            return Some(TargetInfo {
                designer: JEP_RASPI,
                devid: 0,
                part: 0,
                rdp: RdpLevel::Unknown,
            });
        }

        // 跨廠牌辨識：先讀 CoreSight ROM table 的 JEP106 廠商碼（@0xE00FF000）。
        let designer = self.read_designer().await;
        let mut devid = 0u16;
        let mut part = 0u32;
        let mut rdp = RdpLevel::Unknown;

        if designer == JEP_ST || designer == 0 {
            // ST / GD32：DBGMCU_IDCODE @ 0xE0042000，DEV_ID = bits[11:0]，再讀 RDP。
            if let Some(v) = self.read_mem32(0xE004_2000).await {
                let d = (v & 0xFFF) as u16;
                if d != 0 && d != 0xFFF {
                    devid = d;
                    rdp = self.read_rdp(d).await;
                }
            }
        } else if designer == JEP_NORDIC {
            // Nordic：FICR.INFO.PART @ 0x10000100（如 0x52832）。
            part = self.read_mem32(0x1000_0100).await.unwrap_or(0);
        }

        // 已通過 DPIDR+powerup，目標確實存在；即使廠商/型號未知也回報（讓 OLED 顯示廠商或 chip?）。
        Some(TargetInfo {
            designer,
            devid,
            part,
            rdp,
        })
    }

    /// 連線品質量測（供 OLED「訊號儀」，讓使用者照數字接出最佳線路）：
    /// 連讀 16× DPIDR 與 16× AHB(DBGMCU_IDCODE)，回 (dp_ok, ap_ok)，各 0..=16。
    /// 自包含（line reset + JTAG→SWD 切換 + debug powerup）。
    /// - `dp_ok` 反映 DP 層訊號(短交易)：縮線/加電阻時應接近 16。
    /// - `ap_ok` 反映 AHB/AP(長交易，燒錄真正需要的)：訊號變好時往 16 爬。
    ///   若 `dp_ok=16` 但 `ap_ok=0` 且穩定 → 不是 SI，是 RDP1 讀保護（AHB 讀回 0）。
    ///
    /// 僅應在 host 未使用 DAP 時呼叫（會做 line reset）。
    pub async fn link_quality(&mut self) -> (u8, u8) {
        self.line_reset().await;
        self.probe.write_bits(16, 0xE79E).await;
        self.line_reset().await;
        self.probe.write_bits(8, 0).await;

        // DP：連讀 16× DPIDR，與第一筆一致（且非全 0/全 1）才算成功。
        let mut dp_ok = 0u8;
        let mut dp_ref = 0u32;
        let mut have = false;
        for _ in 0..16 {
            let (ack, v) = self.swd_transfer(DP_DPIDR_RD, 0).await;
            if ack == Ack::Ok && v != 0 && v != 0xFFFF_FFFF {
                if !have {
                    dp_ref = v;
                    have = true;
                    dp_ok += 1;
                } else if v == dp_ref {
                    dp_ok += 1;
                }
            }
        }
        if dp_ok == 0 {
            return (0, 0);
        }

        // debug powerup（同 detect_target）。
        let _ = self.transfer_retry(DP_ABORT_WR, 0x1E).await; // ABORT 清 sticky
        let _ = self.transfer_retry(DP_SELECT_WR, 0).await; // SELECT=0
        let _ = self.transfer_retry(DP_CTRLSTAT_WR, 0x5000_0000).await; // CTRL/STAT powerup
        let mut powered = false;
        for _ in 0..20 {
            let (ack, v) = self.transfer_retry(DP_CTRLSTAT_RD, 0).await;
            if ack == Ack::Ok && (v & 0xA000_0000) == 0xA000_0000 {
                powered = true;
                break;
            }
        }
        if !powered {
            return (dp_ok, 0);
        }

        // AP：連讀 16× DBGMCU_IDCODE @0xE0042000，非 0 且一致才算成功。
        let mut ap_ok = 0u8;
        let mut ap_ref = 0u32;
        let mut have_ap = false;
        for _ in 0..16 {
            let Some(v) = self.read_mem32(0xE004_2000).await else {
                continue;
            };
            if v == 0 {
                continue;
            }
            if !have_ap {
                ap_ref = v;
                have_ap = true;
                ap_ok += 1;
            } else if v == ap_ref {
                ap_ok += 1;
            }
        }
        (dp_ok, ap_ok)
    }

    /// 讀 CoreSight ROM table(0xE00FF000) 的 PIDR，取 JEP106 廠商碼（cc<<7|id）；失敗回 0。
    async fn read_designer(&mut self) -> u16 {
        let p1 = self.read_mem32(0xE00F_FFE4).await; // PIDR1
        let p2 = self.read_mem32(0xE00F_FFE8).await; // PIDR2
        let p4 = self.read_mem32(0xE00F_FFD0).await; // PIDR4
        match (p1, p2, p4) {
            (Some(p1), Some(p2), Some(p4)) => {
                let id = ((p2 & 0x7) << 4) | ((p1 >> 4) & 0xF); // 7-bit JEP106 id
                let cc = p4 & 0xF; // continuation count
                (((cc << 7) | id) & 0x7FF) as u16
            }
            _ => 0,
        }
    }

    /// 依 ST DEV_ID 讀 RDP 讀保護等級。讀不到/不支援該家族回 `RdpLevel::Unknown`。
    async fn read_rdp(&mut self, devid: u16) -> RdpLevel {
        // OPTCR/OPTR 的 RDP byte：0xAA=L0、0xCC=L2、其他=L1。
        let rdp_byte = |b: u8| match b {
            0xAA => RdpLevel::Open,
            0xCC => RdpLevel::Level2,
            _ => RdpLevel::Level1,
        };
        match rdp_reg(devid) {
            RdpReg::Optcr => match self.read_mem32(0x4002_3C14).await {
                Some(v) => rdp_byte(((v >> 8) & 0xFF) as u8),
                None => RdpLevel::Unknown,
            },
            RdpReg::Obr => match self.read_mem32(0x4002_201C).await {
                Some(v) if v & 0x2 != 0 => RdpLevel::Level1,
                Some(_) => RdpLevel::Open,
                None => RdpLevel::Unknown,
            },
            RdpReg::Optr => match self.read_mem32(0x4002_2020).await {
                Some(v) => rdp_byte((v & 0xFF) as u8),
                None => RdpLevel::Unknown,
            },
            RdpReg::Unknown => RdpLevel::Unknown,
        }
    }

    /// SWD 喚醒序列（line reset → JTAG→SWD 切換 → line reset → idle），不讀任何暫存器。
    pub async fn swd_wakeup(&mut self) {
        self.line_reset().await;
        self.probe.write_bits(16, 0xE79E).await;
        self.line_reset().await;
        self.probe.write_bits(8, 0).await;
    }

    /// 讀 DPIDR（DP addr0）；**重試多次**,任一成功即回 true（兼作在不在偵測 + 邏輯擷取訊號刺激）。
    /// 重試讓較差的線（4 線/長線/接點劣化）也能讀到,而非單次失敗就判無目標。
    pub async fn swd_read_dpidr(&mut self) -> bool {
        for _ in 0..6 {
            if self.swd_transfer(DP_DPIDR_RD, 0).await.0 == Ack::Ok {
                return true;
            }
        }
        false
    }

    /// 設定 SWCLK 頻率（kHz）。
    pub fn set_swclk_khz(&mut self, khz: u32) {
        self.probe.set_swclk_freq(khz);
    }
    /// 目前 SWCLK 頻率（kHz）。
    pub fn swclk_khz(&self) -> u32 {
        self.probe.freq_khz()
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
