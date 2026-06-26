//! 走線健康判定（WireVerdict + classify + 門檻）。自 main.rs 抽出（Phase 13 R4）。

/// `Unknown` 為初值（尚未掃過，例如 host 在線且預設 build 不監測）→ OLED 沿用既有顯示。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireVerdict {
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
    pub(crate) fn to_u8(self) -> u8 {
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
    pub(crate) fn from_u8(v: u8) -> Self {
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
    pub(crate) fn text(self) -> &'static str {
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
    pub(crate) fn shows_chip(self) -> bool {
        matches!(self, WireVerdict::Ok | WireVerdict::Unknown)
    }
}

// 走線判定門檻（實機可微調）。ce = SWCLK 邊緣數；dp/ap = link_quality 各 0..16 成功數。
#[cfg(feature = "active-detect")]
pub(crate) const EDGE_MIN: u32 = 4; // ce 視為「SWCLK 有在動」的最小邊緣數
#[cfg(feature = "active-detect")]
pub(crate) const DP_GOOD: u32 = 12; // DP 視為穩定
#[cfg(feature = "active-detect")]
pub(crate) const DP_OK: u32 = 14; // OK 門檻
#[cfg(feature = "active-detect")]
pub(crate) const AP_OK: u32 = 14;

/// 依逐線連通 (dio,clk) + 連線品質 (dp,ap) 判定「哪條線/什麼問題」。
///
/// 重點：**SWCLK 是否連通以 `probe_lines()` 的 `clk` 為準（讀目標內部下拉）**，而非 `ce`。
/// `ce`（擷取窗內 SWCLK 邊緣數）只在 `captured`（本輪真的擷取了波形，即 `used!=0`）時才有意義；
/// 當目標完全沒回應（`used==0`）時 `ce` 會被歸 0，**不可**據此判 SWCLK——那是「no target」，
/// 原因可能是 GND/供電/RDP，與 SWCLK 無關。`ce` 僅在「有擷取卻幾乎無邊緣」時當「探針驅動死」的輔助。
#[cfg(feature = "active-detect")]
pub(crate) fn classify(
    lines: crate::state::LineStatus,
    captured: bool,
    ce: u32,
    dp: u32,
    ap: u32,
) -> WireVerdict {
    let crate::state::LineStatus { dio, clk } = lines;
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

