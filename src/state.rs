//! 跨 task 共享狀態（TargetShared/WaveRing/事件環/UART 計數）。自 main.rs 抽出（R4）。
use crate::dap;
use crate::logic;
use crate::wiring::WireVerdict;
use portable_atomic::{AtomicU8, AtomicU32, Ordering};

/// 逐線連通狀態（取代裸 `(bool, bool)`）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineStatus {
    pub dio: bool,
    pub clk: bool,
}

// 跨 task 共享狀態（非阻塞 atomic）。事件用 token ring（環形緩衝，最新覆蓋最舊、無溢出）。
pub(crate) const EVT_N: usize = 3; // 環深度（配合 OLED 顯示行數）
pub(crate) static EVT_RING: [AtomicU8; EVT_N] = [
    AtomicU8::new(0xFF),
    AtomicU8::new(0xFF),
    AtomicU8::new(0xFF),
];
pub(crate) static EVT_SEQ: AtomicU32 = AtomicU32::new(0); // 單調寫入序號（單一寫入者 = dap_task）
pub(crate) static UART_RX_BYTES: AtomicU32 = AtomicU32::new(0); // 目標→host（client log）
pub(crate) static UART_TX_BYTES: AtomicU32 = AtomicU32::new(0); // host→目標

/// layer 2 目標自動偵測 + 連線品質的跨 task 共享狀態（dap_task 單一寫入、oled_task 讀；全 atomic）。
pub(crate) struct TargetShared {
    /// 0 = 無/未偵測；否則 bit31 有效旗標 | 低 12 位 DEV_ID。
    devid: AtomicU32,
    /// 可燒錄狀態（`RdpLevel` as u8）：0=L0,1=L1,2=L2,0xFF=未知/無。
    flash: AtomicU8,
    /// JEP106 廠商碼 + 廠商專屬 part（Nordic FICR 等）。
    designer: AtomicU32,
    part: AtomicU32,
    /// CPUID PARTNO（Cortex-M 核心型號）；0=未知。
    core: AtomicU32,
    /// 自適應偵測實際採用的 SWCLK（kHz，本輪操作/測試clk）；0 = 沒讀到。
    used_khz: AtomicU32,
    /// 已確認 AP 穩的 SWCLK（kHz，穩定clk）：上升式選速退階後釘住的可靠速率。
    stable_khz: AtomicU32,
    /// 連線品質訊號儀：每輪 16 次讀取成功數（DP 短交易 / AP AHB 長交易）。
    link_dp: AtomicU32,
    link_ap: AtomicU32,
    /// 目前正在嘗試的偵測 SWCLK（kHz）：掃頻每步更新。OLED 無目標時顯示，反映掃到哪個頻率。
    probe_khz: AtomicU32,
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
    pub(crate) const fn new() -> Self {
        Self {
            devid: AtomicU32::new(0),
            flash: AtomicU8::new(0xFF),
            designer: AtomicU32::new(0),
            part: AtomicU32::new(0),
            core: AtomicU32::new(0),
            used_khz: AtomicU32::new(0),
            stable_khz: AtomicU32::new(0),
            link_dp: AtomicU32::new(0),
            link_ap: AtomicU32::new(0),
            probe_khz: AtomicU32::new(0),
            dio_conn: AtomicU8::new(0),
            clk_conn: AtomicU8::new(0),
            verdict: AtomicU8::new(0),
            clk_flaps: AtomicU32::new(0),
            dio_flaps: AtomicU32::new(0),
        }
    }
    /// 記錄逐線連通結果。
    pub(crate) fn set_lines(&self, l: LineStatus) {
        self.dio_conn.store(l.dio as u8, Ordering::Relaxed);
        self.clk_conn.store(l.clk as u8, Ordering::Relaxed);
    }
    pub(crate) fn lines(&self) -> LineStatus {
        LineStatus {
            dio: self.dio_conn.load(Ordering::Relaxed) != 0,
            clk: self.clk_conn.load(Ordering::Relaxed) != 0,
        }
    }
    pub(crate) fn set_verdict(&self, v: WireVerdict) {
        self.verdict.store(v.to_u8(), Ordering::Relaxed);
    }
    pub(crate) fn verdict(&self) -> WireVerdict {
        WireVerdict::from_u8(self.verdict.load(Ordering::Relaxed))
    }
    pub(crate) fn bump_clk_flap(&self) {
        self.clk_flaps.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn bump_dio_flap(&self) {
        self.dio_flaps.fetch_add(1, Ordering::Relaxed);
    }
    /// 回 (clk_flaps, dio_flaps)。
    pub(crate) fn flaps(&self) -> (u32, u32) {
        (
            self.clk_flaps.load(Ordering::Relaxed),
            self.dio_flaps.load(Ordering::Relaxed),
        )
    }
    /// 記錄掃頻目前嘗試的速率（kHz）。
    pub(crate) fn set_probe_khz(&self, khz: u32) {
        self.probe_khz.store(khz, Ordering::Relaxed);
    }
    pub(crate) fn probe_khz(&self) -> u32 {
        self.probe_khz.load(Ordering::Relaxed)
    }
    /// 寫入偵測結果（designer/part/flash 先寫，devid 含有效旗標最後寫）。
    pub(crate) fn store(&self, info: &dap::TargetInfo) {
        self.designer.store(info.designer as u32, Ordering::Relaxed);
        self.part.store(info.part, Ordering::Relaxed);
        self.core.store(info.core as u32, Ordering::Relaxed);
        self.flash.store(info.rdp.to_u8(), Ordering::Relaxed);
        self.devid
            .store(Self::VALID | info.devid as u32, Ordering::Relaxed);
    }
    /// 清除（無目標）。
    pub(crate) fn clear(&self) {
        self.devid.store(0, Ordering::Relaxed);
        self.flash.store(0xFF, Ordering::Relaxed);
    }
    pub(crate) fn valid(&self) -> bool {
        self.devid.load(Ordering::Relaxed) & Self::VALID != 0
    }
    pub(crate) fn devid(&self) -> u16 {
        (self.devid.load(Ordering::Relaxed) & 0xFFF) as u16
    }
    pub(crate) fn designer(&self) -> u16 {
        self.designer.load(Ordering::Relaxed) as u16
    }
    pub(crate) fn part(&self) -> u32 {
        self.part.load(Ordering::Relaxed)
    }
    pub(crate) fn core(&self) -> u16 {
        self.core.load(Ordering::Relaxed) as u16
    }
    pub(crate) fn rdp(&self) -> dap::RdpLevel {
        dap::RdpLevel::from_u8(self.flash.load(Ordering::Relaxed))
    }
    pub(crate) fn set_used_khz(&self, khz: u32) {
        self.used_khz.store(khz, Ordering::Relaxed);
    }
    pub(crate) fn used_khz(&self) -> u32 {
        self.used_khz.load(Ordering::Relaxed)
    }
    pub(crate) fn set_stable_khz(&self, khz: u32) {
        self.stable_khz.store(khz, Ordering::Relaxed);
    }
    pub(crate) fn stable_khz(&self) -> u32 {
        self.stable_khz.load(Ordering::Relaxed)
    }
    pub(crate) fn set_link(&self, q: &dap::LinkQuality) {
        self.link_dp.store(q.dp as u32, Ordering::Relaxed);
        self.link_ap.store(q.ap as u32, Ordering::Relaxed);
    }
    /// 連線品質（dp/ap 成功數，0..=16）。
    pub(crate) fn link(&self) -> dap::LinkQuality {
        dap::LinkQuality {
            dp: self.link_dp.load(Ordering::Relaxed) as u8,
            ap: self.link_ap.load(Ordering::Relaxed) as u8,
        }
    }
}

pub(crate) static TARGET: TargetShared = TargetShared::new();

/// SWD 數位邏輯波形 token ring（環形緩衝、捲動式）：`WAVE_COLS` 欄各 1 bit，兩通道(SWCLK/SWDIO)。
/// 每輪擷取把新片段「推進」ring（最新覆蓋最舊、不積壓），OLED 由 pos 起讀（最舊→最新）→ 視覺流動。
pub(crate) const WAVE_COLS: usize = 128;
pub(crate) const WAVE_PUSH: usize = 32; // 每輪推進的欄數（捲動速度）

pub(crate) struct WaveRing {
    clk: [AtomicU32; 4],
    dio: [AtomicU32; 4],
    pos: AtomicU32, // 下一個寫入欄（= 最舊欄）
}

impl WaveRing {
    pub(crate) const fn new() -> Self {
        Self {
            clk: [const { AtomicU32::new(0) }; 4],
            dio: [const { AtomicU32::new(0) }; 4],
            pos: AtomicU32::new(0),
        }
    }
    pub(crate) fn set_bit(arr: &[AtomicU32; 4], col: usize, bit: bool) {
        let wi = col / 32;
        let m = 1u32 << (col % 32);
        let mut v = arr[wi].load(Ordering::Relaxed);
        v = if bit { v | m } else { v & !m };
        arr[wi].store(v, Ordering::Relaxed); // 單一寫入者(dap_task)，load/store 即可
    }
    /// 從擷取緩衝找出時脈片段，取乾淨的 WAVE_PUSH 欄(1:1，不混疊)推進 ring。
    pub(crate) fn push(&self, buf: &[u32]) {
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
    pub(crate) fn push_flat(&self) {
        let mut pos = self.pos.load(Ordering::Relaxed) as usize;
        for _ in 0..WAVE_PUSH {
            Self::set_bit(&self.clk, pos, false);
            Self::set_bit(&self.dio, pos, false);
            pos = (pos + 1) % WAVE_COLS;
        }
        self.pos.store(pos as u32, Ordering::Relaxed);
    }
    pub(crate) fn snapshot(arr: &[AtomicU32; 4]) -> [u32; 4] {
        [
            arr[0].load(Ordering::Relaxed),
            arr[1].load(Ordering::Relaxed),
            arr[2].load(Ordering::Relaxed),
            arr[3].load(Ordering::Relaxed),
        ]
    }
    pub(crate) fn load_clk(&self) -> [u32; 4] {
        Self::snapshot(&self.clk)
    }
    pub(crate) fn load_dio(&self) -> [u32; 4] {
        Self::snapshot(&self.dio)
    }
    pub(crate) fn pos(&self) -> usize {
        self.pos.load(Ordering::Relaxed) as usize
    }
}

pub(crate) static WAVE: WaveRing = WaveRing::new();

/// 記錄一筆 DAP 事件（dap_task 單一寫入者）。
pub(crate) fn record_evt(id: u8) {
    let s = EVT_SEQ.load(Ordering::Relaxed);
    EVT_RING[(s as usize) % EVT_N].store(id, Ordering::Relaxed);
    EVT_SEQ.store(s.wrapping_add(1), Ordering::Relaxed);
}

/// 取第 i 新的事件名稱（i=0 最新）；無則回空字串。
/// （OLED 改為波形圖後暫不顯示事件；保留供日後用。）
#[allow(dead_code)]
pub(crate) fn evt_name(i: u32) -> &'static str {
    let seq = EVT_SEQ.load(Ordering::Relaxed);
    if seq > i {
        let idx = ((seq - 1 - i) as usize) % EVT_N;
        dap::cmd_name(EVT_RING[idx].load(Ordering::Relaxed))
    } else {
        ""
    }
}

