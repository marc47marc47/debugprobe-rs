//! 自主掃描：adaptive_sweep + idle_scan。自 main.rs 抽出（Phase 13 R5）。
use crate::state::{LineStatus, TARGET, WAVE};
use crate::wiring::{WireVerdict, classify};
use crate::{dap, logic};
use embassy_time::{Duration, Timer};

/// verdict 遲滯：新結論需連續這麼多輪相同才更新 OLED 顯示（消除臨界線 1Hz 閃爍）。
const VERDICT_DEBOUNCE: u8 = 3;

/// 自主掃描的跨輪持久狀態（取代 idle_scan 原本散裝的 3 個 `&mut` 參數）。
pub(crate) struct ScanState {
    /// 黏著速率（kHz）：鎖在上次能通的 SWCLK，避免每輪重掃造成顯示亂跳。
    sticky: u32,
    /// 連續取樣不到次數（拔除 hysteresis）。
    absent: u32,
    /// 上輪逐線連通（鬆動 flap 統計用）。
    prev: Option<LineStatus>,
    /// verdict 遲滯：上輪原始結論 + 已連續相同的輪數。
    last_raw: WireVerdict,
    streak: u8,
}

impl ScanState {
    pub(crate) const fn new() -> Self {
        Self {
            sticky: 1000,
            absent: 0,
            prev: None,
            last_raw: WireVerdict::Unknown,
            streak: 0,
        }
    }
}

#[cfg(feature = "active-detect")]
pub(crate) async fn adaptive_sweep(dap: &mut dap::Dap<'static>, sticky: &mut u32) -> u32 {
    // 純 single-drop（只讀 DPIDR、**絕不寫 TARGETSEL**）：先試黏著速率，再由快到慢掃。
    // 不做 multidrop——TARGETSEL 會把 DPv2 STM32 誤 deselect 且不可逆，整顆從此偵測不到。
    TARGET.set_probe_khz(*sticky);
    dap.set_swclk_khz(*sticky);
    dap.swd_wakeup().await;
    if dap.swd_read_dpidr().await {
        return *sticky;
    }
    for &khz in &[1000u32, 500, 250, 100, 50, 20] {
        TARGET.set_probe_khz(khz);
        dap.set_swclk_khz(khz);
        dap.swd_wakeup().await;
        if dap.swd_read_dpidr().await {
            *sticky = khz; // 鎖定新速率
            return khz;
        }
    }
    0
}

/// host 閒置時的一輪自主掃描：自適應掃頻 → 擷取波形 → 偵測晶片 → 量連線品質；更新 `TARGET`/`WAVE`。
/// 全程 save/restore SWCLK，不留痕跡給 host。
#[cfg(feature = "active-detect")]
pub(crate) async fn idle_scan(
    dap: &mut dap::Dap<'static>,
    cap: &mut logic::LogicCapture<'static>,
    st: &mut ScanState,
) {
    use embassy_futures::select::select;
    let saved_khz = dap.swclk_khz();

    // 走線監測第一步：逐線連通（drive 反向→釋放→讀目標內部 pull）。判斷哪條線實體斷掉。
    let (dio, clk) = dap.probe_lines().await;
    let lines = LineStatus { dio, clk };
    TARGET.set_lines(lines);
    // 鬆動統計：與上輪比較，連通狀態翻轉即累加（反覆拔插時，flap 最多者 = 最會鬆的線）。
    if let Some(prev) = st.prev {
        if lines.dio != prev.dio {
            TARGET.bump_dio_flap();
        }
        if lines.clk != prev.clk {
            TARGET.bump_clk_flap();
        }
    }
    st.prev = Some(lines);

    let used = adaptive_sweep(dap, &mut st.sticky).await;
    TARGET.set_used_khz(used);

    let mut buf = [0u32; logic::CAP_WORDS];
    let ce;
    let (dp, ap);
    if used != 0 {
        st.absent = 0;
        // 偵測到目標(可用速率)才擷取波形 + 量邊緣(此時 SWCLK 在跑、量得到真訊號)。
        cap.start();
        let xfer = cap.dma_into(&mut buf);
        let _ = dap.swd_read_dpidr().await; // 訊號刺激
        let _ = select(xfer, Timer::after(Duration::from_millis(20))).await;
        cap.stop();
        // count_signal 的 ce 仍供 classify 當「探針 SWCLK 驅動死」輔助；其餘統計不再顯示。
        ce = logic::count_signal(&buf).clk_edges;
        WAVE.push(&buf);
        // 晶片偵測只在尚未鎖定時做一次（F2 功能保留）。
        if !TARGET.valid()
            && let Some(info) = dap.detect_target().await
        {
            TARGET.store(&info);
        }
        let q = dap.link_quality().await;
        dp = q.dp as u32;
        ap = q.ap as u32;
        TARGET.set_link(&q);
    } else {
        // 無目標：不擷取(低速擷取無意義)、推平線、歸零。
        WAVE.push_flat();
        TARGET.set_link(&dap::LinkQuality { dp: 0, ap: 0 });
        st.absent += 1;
        if st.absent >= 2 {
            TARGET.clear();
        }
        ce = 0;
        dp = 0;
        ap = 0;
    }
    // 走線判定：彙整逐線連通 + 連線品質 → 結論（供 OLED 即時顯示哪條線壞）。
    // captured = 本輪是否真的擷取了波形（used!=0）；只有此時 ce 才有意義。
    // verdict 遲滯：原始結論需連續 VERDICT_DEBOUNCE 輪相同才更新顯示，
    // 否則臨界線(AP 0↔16 抖)會讓 OLED 每秒亂跳。穩定後才換，顯示=「目前大致狀態」。
    let raw = classify(lines, used != 0, ce, dp, ap);
    if raw == st.last_raw {
        st.streak = st.streak.saturating_add(1);
    } else {
        st.last_raw = raw;
        st.streak = 1;
    }
    if st.streak >= VERDICT_DEBOUNCE {
        TARGET.set_verdict(raw);
    }
    dap.set_swclk_khz(saved_khz); // 還原 host 設定
}

