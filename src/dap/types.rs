//! DAP 核心的型別與常數（enum/struct/命名常數/查表助手）。
//! 自 `dap/mod.rs` 抽出（Phase 13 R2），與 SWD 傳輸/偵測邏輯解耦。

// --- 能力 / 設定常數（對應 DAP_config.h）---
pub(crate) const DAP_PACKET_SIZE: u16 = 64;
pub(crate) const DAP_PACKET_COUNT: u8 = 8;
pub(crate) const CAPABILITIES: u8 = 0x01; // bit0 = SWD, bit1 = JTAG(0)
pub(crate) const FW_VERSION: &str = "2.1.0";

// --- 回應狀態 ---
pub(crate) const DAP_OK: u8 = 0x00;
pub(crate) const ID_DAP_INVALID: u8 = 0xFF;

/// DAP 命令 ID（取代 ID_DAP_* 常數）。值 = CMSIS-DAP 命令位元組；未列出者 = 不支援(JTAG/SWO/Vendor)。
#[derive(Clone, Copy)]
pub(crate) enum DapCmd {
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
    pub(crate) fn from_u8(v: u8) -> Option<Self> {
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
pub(crate) const INFO_VENDOR: u8 = 0x01;
pub(crate) const INFO_PRODUCT: u8 = 0x02;
pub(crate) const INFO_SERIAL: u8 = 0x03;
pub(crate) const INFO_FW_VER: u8 = 0x04;
pub(crate) const INFO_CAPABILITIES: u8 = 0xF0;
pub(crate) const INFO_PACKET_COUNT: u8 = 0xFE;
pub(crate) const INFO_PACKET_SIZE: u8 = 0xFF;

// --- SWD transfer request bits ---
pub(crate) const REQ_APND_P: u8 = 1 << 0;
pub(crate) const REQ_RNW: u8 = 1 << 1;

/// SWD 傳輸的 ACK 結果（取代裸 u8）：線上 3-bit ACK + 本地 parity/協定錯誤。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ack {
    Ok,       // 線上 0b001
    Wait,     // 線上 0b010
    Fault,    // 線上 0b100
    Parity,   // Ack::Ok 後讀資料相 parity 錯誤（本地）
    Protocol, // 其他（無回應/協定錯誤）
}

impl Ack {
    /// 線上 3-bit ACK → Ack（1=OK、2=WAIT、4=FAULT、其餘=協定錯誤）。
    pub(crate) fn from_swd(bits: u8) -> Self {
        match bits & 0x7 {
            1 => Ack::Ok,
            2 => Ack::Wait,
            4 => Ack::Fault,
            _ => Ack::Protocol,
        }
    }
    /// 回傳給 host 的 DAP_Transfer 回應 ACK byte（與原本裸值一致：OK=1/WAIT=2/FAULT=4/parity=8/其他=7）。
    pub(crate) fn to_byte(self) -> u8 {
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
pub(crate) const fn swd_req(ap: bool, rnw: bool, a23: u8) -> u8 {
    (ap as u8) | ((rnw as u8) << 1) | ((a23 & 0x3) << 2)
}

// 韌體內部自主存取用的具名 DP/AP 請求（取代散落的裸 request byte 魔術數）。
pub(crate) const DP_DPIDR_RD: u8 = swd_req(false, true, 0b00); // DP 讀 DPIDR（addr 0x0）
pub(crate) const DP_ABORT_WR: u8 = swd_req(false, false, 0b00); // DP 寫 ABORT（addr 0x0）
pub(crate) const DP_CTRLSTAT_WR: u8 = swd_req(false, false, 0b01); // DP 寫 CTRL/STAT（addr 0x4）
pub(crate) const DP_CTRLSTAT_RD: u8 = swd_req(false, true, 0b01); // DP 讀 CTRL/STAT（addr 0x4）
pub(crate) const DP_SELECT_WR: u8 = swd_req(false, false, 0b10); // DP 寫 SELECT（addr 0x8）
pub(crate) const DP_RDBUFF_RD: u8 = swd_req(false, true, 0b11); // DP 讀 RDBUFF（addr 0xC）
pub(crate) const AP_CSW_WR: u8 = swd_req(true, false, 0b00); // AP 寫 CSW（addr 0x0）
pub(crate) const AP_TAR_WR: u8 = swd_req(true, false, 0b01); // AP 寫 TAR（addr 0x4）
pub(crate) const AP_DRW_RD: u8 = swd_req(true, true, 0b11); // AP 讀 DRW（addr 0xC）

/// DAP 命令 ID → 名稱（供 OLED 活動顯示）。
pub fn cmd_name(id: u8) -> &'static str {
    match DapCmd::from_u8(id) {
        Some(c) => c.name(),
        None => "?",
    }
}

/// 讀保護（RDP）等級。跨 task 以 `to_u8`/`from_u8` 經 atomic 傳遞；`short()` 供 OLED 顯示。
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
    /// OLED「可燒狀態」短標（左側面板有限寬度用，不撞右側柱狀圖）。
    pub fn short(self) -> &'static str {
        match self {
            RdpLevel::Open => "RDP0",
            RdpLevel::Level1 => "RDP1",
            RdpLevel::Level2 => "RDP2",
            RdpLevel::Unknown => "RDP?",
        }
    }
}

/// 連線品質量測結果（OLED 訊號儀）：每輪 16 次讀取中成功的次數。
pub struct LinkQuality {
    /// DP（短交易）讀成功數 0..=16；訊號稍差也容易滿。
    pub dp: u8,
    /// AP（AHB 長交易，燒錄真正需要）讀成功數 0..=16；訊號變好時往 16 爬。
    pub ap: u8,
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
    /// CPUID PARTNO（Cortex-M 核心：0xC20=M0,0xC60=M0+,0xC23=M3,0xC24=M4,0xC27=M7,
    /// 0xD20=M23,0xD21=M33…）；0=未知。通用核心辨識用。
    pub core: u16,
}

/// 常見 JEP106 廠商碼（cc<<7 | 7-bit id）。
pub const JEP_ST: u16 = 0x020; // STMicroelectronics（GD32 亦複製此 ROM table 廠商碼）
pub const JEP_NORDIC: u16 = 0x244; // Nordic Semiconductor
pub const JEP_RASPI: u16 = 0x493; // Raspberry Pi

/// 讀保護（RDP）所在的 option 暫存器家族。各家族暫存器位址/格式不同。
pub(crate) enum RdpReg {
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
pub(crate) fn rdp_reg(devid: u16) -> RdpReg {
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

pub(crate) fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
pub(crate) fn put_u32_le(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_le_bytes());
}
