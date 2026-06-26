//! 自主目標偵測（layer-2 晶片型號 / 連線品質 / 走線；自 mod.rs 抽出，R2）。
use super::Dap;
use super::types::*;

impl<'d> Dap<'d> {
    // ---------------- 自主目標偵測（供 OLED 顯示 layer 2 晶片型號）----------------

    /// SWD line reset：>=50 個 SWCLK 週期、SWDIO 持高。
    async fn line_reset(&mut self) {
        self.probe.write_bits(32, 0xFFFF_FFFF).await;
        self.probe.write_bits(32, 0xFFFF_FFFF).await; // 共 64 高，足夠
    }

    /// 喚醒單核 DP：JTAG→SWD 切換 + line reset（`swd_wakeup`）+ 讀 DPIDR。讀到回 true。
    /// 刻意**只用 JTAG-switch**（不送 dormant 序列，避免把單核目標誤推進 dormant 態）。
    async fn swd_connect(&mut self) -> bool {
        self.swd_wakeup().await;
        self.swd_read_dpidr().await
    }

    /// debug powerup：清 sticky → SELECT=0 → CTRL/STAT PWRUPREQ → 輪詢 powerup ACK。成功回 true。
    /// （detect_target 與 link_quality 共用，取代原本兩處逐字重複的序列。）
    async fn debug_powerup(&mut self) -> bool {
        let _ = self.transfer_retry(DP_ABORT_WR, reg::DP_ABORT_CLEAR).await; // 清 sticky error
        let _ = self.transfer_retry(DP_SELECT_WR, 0).await; // SELECT = 0（APSEL0, bank0）
        let _ = self
            .transfer_retry(DP_CTRLSTAT_WR, reg::CTRLSTAT_PWRUPREQ)
            .await; // CSYS/CDBG PWRUPREQ
        // 輪詢 powerup ACK（CDBGPWRUPACK bit29 | CSYSPWRUPACK bit31）後才能存取 AP。
        for _ in 0..20 {
            let (ack, v) = self.transfer_retry(DP_CTRLSTAT_RD, 0).await;
            if ack == Ack::Ok && (v & reg::CTRLSTAT_PWRUPACK) == reg::CTRLSTAT_PWRUPACK {
                return true;
            }
        }
        false
    }

    /// 讀 CPUID 的 PARTNO(bits[15:4])（A：通用 Cortex-M 核心辨識）；失敗/讀不穩回 0。
    async fn read_cpuid_part(&mut self) -> u16 {
        // CPUID 不可能為 0 → allow_zero=false（0 視為壞讀）。
        match self.read_mem32_stable(reg::CPUID, false).await {
            Some(v) => ((v >> 4) & 0xFFF) as u16,
            None => 0,
        }
    }

    /// 對 marginal AP：同址重讀，需「連續兩次一致」才採信，抗偶發位元錯(如 CPUID 0xC23→0xC24
    /// 把 M3 誤判成 M4)與讀 0。`allow_zero=false` 時把 0 也當壞讀(CPUID/DBGMCU 永不為 0);
    /// PIDR/option 暫存器合法可為 0 故 allow_zero=true。全 1(0xFFFFFFFF)一律視為壞讀。最多 5 次。
    async fn read_mem32_stable(&mut self, addr: u32, allow_zero: bool) -> Option<u32> {
        let mut prev: Option<u32> = None;
        for _ in 0..5 {
            if let Some(v) = self.read_mem32(addr).await {
                if v == 0xFFFF_FFFF || (!allow_zero && v == 0) {
                    continue; // 壞讀，重置不算數
                }
                if prev == Some(v) {
                    return Some(v); // 連續兩次一致 → 採信
                }
                prev = Some(v);
            }
        }
        None
    }

    /// 經 AHB-AP 讀一個 32-bit 記憶體字（posted read + RDBUFF）。全程 WAIT 重試。
    async fn read_mem32(&mut self, addr: u32) -> Option<u32> {
        // AP CSW = 32-bit word、single（probe-rs/openocd 對 STM32 常用值）
        if self.transfer_retry(AP_CSW_WR, reg::AP_CSW_32BIT).await.0 != Ack::Ok {
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
        // 喚醒：JTAG→SWD 切換 + 讀 DPIDR（single-drop）。不做 multidrop（避免 TARGETSEL 誤刪 DPv2 STM32）。
        if !self.swd_connect().await {
            return None;
        }
        if !self.debug_powerup().await {
            return None;
        }

        // A：CPUID 通用核心辨識（任何 Cortex-M 都讀得到）。
        let core = self.read_cpuid_part().await;

        // 跨廠牌辨識：先讀 CoreSight ROM table 的 JEP106 廠商碼（@0xE00FF000）。
        let designer = self.read_designer().await;
        let mut devid = 0u16;
        let mut part = 0u32;
        let mut rdp = RdpLevel::Unknown;

        if designer == JEP_ST || designer == 0 {
            // ST / GD32：DBGMCU_IDCODE，DEV_ID = bits[11:0]（一致性讀），再讀 RDP。
            if let Some(v) = self.read_mem32_stable(reg::DBGMCU_IDCODE, false).await {
                let d = (v & 0xFFF) as u16;
                if d != 0 && d != 0xFFF {
                    devid = d;
                    rdp = self.read_rdp(d).await;
                }
            }
        } else if designer == JEP_NORDIC {
            // Nordic：FICR.INFO.PART（如 0x52832）。
            part = self.read_mem32_stable(reg::NORDIC_FICR_PART, false).await.unwrap_or(0);
        }

        // 全垃圾守門：devid/part/core 全 0（marginal AP 讀壞/讀 0）→ 回 None，別鎖「vendor 0x000」之類。
        // idle_scan 會下一輪重試，直到一次乾淨讀。core 由一致性讀取得，已抗單 bit 錯(M3→M4)。
        if devid == 0 && part == 0 && core == 0 {
            return None;
        }

        // 通過守門：至少有一項可信。即使部分未知也回報（OLED 顯示晶片名或核心；缺的由再驗升級）。
        Some(TargetInfo {
            designer,
            devid,
            part,
            rdp,
            core,
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
    pub async fn link_quality(&mut self) -> LinkQuality {
        self.swd_wakeup().await;

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
            return LinkQuality { dp: 0, ap: 0 };
        }

        // debug powerup（同 detect_target，共用 helper）。
        if !self.debug_powerup().await {
            return LinkQuality { dp: dp_ok, ap: 0 };
        }

        // AP：連讀 16× DBGMCU_IDCODE，非 0 且一致才算成功。
        let mut ap_ok = 0u8;
        let mut ap_ref = 0u32;
        let mut have_ap = false;
        for _ in 0..16 {
            let Some(v) = self.read_mem32(reg::DBGMCU_IDCODE).await else {
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
        LinkQuality { dp: dp_ok, ap: ap_ok }
    }

    /// 讀 CoreSight ROM table(0xE00FF000) 的 PIDR，取 JEP106 廠商碼（cc<<7|id）；失敗回 0。
    async fn read_designer(&mut self) -> u16 {
        // PIDR4 對 ST 合法為 0（JEP106 continuation=0）→ allow_zero=true。
        let p1 = self.read_mem32_stable(reg::ROM_PIDR1, true).await;
        let p2 = self.read_mem32_stable(reg::ROM_PIDR2, true).await;
        let p4 = self.read_mem32_stable(reg::ROM_PIDR4, true).await;
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
            RdpReg::Optcr => match self.read_mem32_stable(reg::FLASH_OPTCR, true).await {
                Some(v) => rdp_byte(((v >> 8) & 0xFF) as u8),
                None => RdpLevel::Unknown,
            },
            RdpReg::Obr => match self.read_mem32_stable(reg::FLASH_OBR, true).await {
                Some(v) if v & 0x2 != 0 => RdpLevel::Level1,
                Some(_) => RdpLevel::Open,
                None => RdpLevel::Unknown,
            },
            RdpReg::Optr => match self.read_mem32_stable(reg::FLASH_OPTR, true).await {
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
        for _ in 0..12 {
            // D：重試加倍（爛線/長線/接點劣化也撐到底）。
            if self.swd_transfer(DP_DPIDR_RD, 0).await.0 == Ack::Ok {
                return true;
            }
        }
        false
    }
}
