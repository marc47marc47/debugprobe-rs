//! 自主掃描：adaptive_sweep + idle_scan。自 main.rs 抽出（Phase 13 R5）。
use crate::state::{LineStatus, TARGET, WAVE};
use crate::wiring::{WireVerdict, classify};
use crate::{dap, logic};
use embassy_time::{Duration, Instant, Timer};

/// verdict 遲滯：新結論需連續這麼多輪相同才更新 OLED 顯示（消除臨界線 1Hz 閃爍）。
const VERDICT_DEBOUNCE: u8 = 3;

/// SWCLK 速度階梯（kHz，由低往上爬）。低速最穩，先在低速建立連線/讀晶片，再往上試效能。
const SPEED_STEPS: [u32; 7] = [10, 30, 50, 100, 250, 500, 1000];

/// AP 連線品質低於此值（link_quality 16 次中成功數）視為「此速率 AP 撐不住」。
/// 實測本線：100kHz AP 滿(16)、250kHz 撐 2 秒後崩成 0~8 → 12 可乾淨分辨。
const AP_DEMOTE: u32 = 12;
/// 連續這麼多輪 AP 撐不住才降速並把上限釘在此速以下（避免單輪雜訊誤降）。
const DEMOTE_AFTER: u8 = 2;
/// 定型後停在某速這麼久就解開上限、再往上試一階（防一次瞬斷把上限永久釘死；=「閒置12秒再往上測」）。
const REPROBE_SECS: u64 = 12;

/// 階梯往上一階（到頂不變）。
fn step_up(khz: u32) -> u32 {
    let mut prev = khz;
    for &s in &SPEED_STEPS {
        if s > khz {
            return s;
        }
        prev = s;
    }
    prev
}

/// 階梯往下一階（到底不變）。
fn step_down(khz: u32) -> u32 {
    let mut lower = khz;
    for &s in &SPEED_STEPS {
        if s >= khz {
            break;
        }
        lower = s;
    }
    lower
}


/// 自主掃描的跨輪持久狀態（取代 idle_scan 原本散裝的 3 個 `&mut` 參數）。
pub(crate) struct ScanState {
    /// 本輪操作速率（kHz，測試clk）：由低往上爬。DP/AP 撐不住就退階。
    cur: u32,
    /// 已確認 AP 穩的速率（kHz，穩定clk）：供顯示與 host 夾速(clamp)用。
    stable: u32,
    /// 容許往上爬的上限（kHz）：某速 DP/AP 撐不住就把上限釘在其下，不再往上試（防抖）。
    ceiling: u32,
    /// 是否已「定型」：找到上限(第一次降速)後設 true → 不再每輪亂爬，只靠 12 秒重測往上。
    settled: bool,
    /// 連續 AP 撐不住的輪數（夠多才降速）。
    bad: u8,
    /// 上次「解鎖往上重測」的時間；停在某速超過 REPROBE_SECS 就再往上試一階。
    last_probe: Option<Instant>,
    /// 連續取樣不到次數（拔除 hysteresis）。
    absent: u32,
    /// 上輪逐線連通（鬆動 flap 統計用）。
    prev: Option<LineStatus>,
    /// verdict 遲滯：上輪原始結論 + 已連續相同的輪數。
    last_raw: WireVerdict,
    streak: u8,
    /// 上輪 DP 連線品質（0..16）。供 probe_lines 閘控：DP 還在(>0)就別驅動測線打擾連線。
    last_dp: u32,
}

impl ScanState {
    pub(crate) const fn new() -> Self {
        Self {
            cur: 10,      // 由最低速起步（最穩，先把晶片讀對）
            stable: 10,
            ceiling: 1000, // 初始不設限，往上爬遇到撐不住才往下釘
            settled: false,
            bad: 0,
            last_probe: None,
            absent: 0,
            prev: None,
            last_raw: WireVerdict::Unknown,
            streak: 0,
            last_dp: 0,
        }
    }
}

#[cfg(feature = "active-detect")]
pub(crate) async fn adaptive_sweep(dap: &mut dap::Dap<'static>, st: &mut ScanState) -> u32 {
    // 純 single-drop（只讀 DPIDR、**絕不寫 TARGETSEL**）：在目前操作速率 cur 建立連線；
    // 若 cur 連 DP 都不通（太快或無目標）→ 把上限釘在此速、往下退一階重試，直到通或到最低。
    // 不做 multidrop——TARGETSEL 會把 DPv2 STM32 誤 deselect 且不可逆，整顆從此偵測不到。
    loop {
        TARGET.set_probe_khz(st.cur);
        dap.set_swclk_khz(st.cur);
        dap.swd_wakeup().await;
        match dap.read_dpidr_val().await {
            // single-drop 讀到 DPIDR：值是 RP2040 的就設旗標(走 multidrop)，否則 STM32(single-drop)。
            Some(v) => {
                dap.rp2040 = v == dap::reg::RP2040_DPIDR;
                return st.cur;
            }
            // single-drop 讀不到：可能是 RP2040（multidrop 需 TARGETSEL）→ 試選 core0。
            // 只在 single-drop 失敗後才送 TARGETSEL → STM32 present(single-drop 必成功)不受影響。
            None => {
                if dap.try_rp2040_select().await {
                    dap.rp2040 = true;
                    return st.cur;
                }
            }
        }
        let lower = step_down(st.cur);
        if lower == st.cur {
            dap.rp2040 = false;
            return 0; // 已到最低速仍不通 → 視為無目標
        }
        st.ceiling = st.cur; // 此速及以上 DP 不通 → 釘上限，別再往上爬
        st.cur = lower;
    }
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
    // 但 probe_lines 會停 SM、把 SWDIO/SWCLK 當 GPIO 主動驅動 → 會打擾正在進行的 SWD 連線、
    // 把目標 AP 打掉（實測：連線正常時每輪戳一次，AP 約 2 秒後崩成 PWR/AP fail）。
    // 因此只在「上輪 DP 掛掉(=真的無 SWD 回應)」時才驅動測線找斷線；DP 還在就視為兩線皆通、不打擾。
    let lines = if st.last_dp > 0 {
        LineStatus { dio: true, clk: true }
    } else {
        let (dio, clk) = dap.probe_lines().await;
        LineStatus { dio, clk }
    };
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

    let used = adaptive_sweep(dap, st).await;
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
        // 晶片偵測只在未鎖定時做一次。先前的「每輪重驗升級」會在 marginal 線上狂灌 AP 讀取流量、
        // 把鏈路操到連 DP 都掛(=0) → 已移除（要補 RDP?/core 改用低頻率限流再驗，非每輪）。
        if !TARGET.valid()
            && let Some(info) = dap.detect_target().await
        {
            TARGET.store(&info);
        }
        let q = dap.link_quality().await;
        dp = q.dp as u32;
        ap = q.ap as u32;
        st.last_dp = dp; // 供下輪 probe_lines 閘控
        TARGET.set_link(&q);

        // 選速：定型前快速往上找上限；定型後「釘住不動」，只每 REPROBE_SECS 秒往上試一階。
        // 關鍵：定型後不再每輪換速（每輪換速會擾動臨界線、AP 一抖就降 → 來回跳）。
        let now = Instant::now();
        // 定型後，閒置夠久才解鎖、允許往上試一階（對應「閒置 12 秒再往上測」）。
        let reprobe = st.settled
            && st
                .last_probe
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(REPROBE_SECS));
        if reprobe {
            st.ceiling = SPEED_STEPS[SPEED_STEPS.len() - 1]; // 解開上限
            st.last_probe = Some(now);
        }
        // 何時可往上爬：未定型(快速找上限) 或 這輪剛解鎖重測。
        let may_climb = !st.settled || reprobe;

        if ap < AP_DEMOTE {
            // 此速 AP 撐不住 → 連續夠多輪就降速、把上限釘在此速以下，並標記已定型(找到上限)。
            st.bad = st.bad.saturating_add(1);
            if st.bad >= DEMOTE_AFTER {
                st.ceiling = st.cur;
                st.cur = step_down(st.cur);
                st.bad = 0;
                st.settled = true;
            }
        } else {
            st.bad = 0;
            st.stable = st.cur; // 此速 AP 穩 → 記為穩定值
            if may_climb {
                let up = step_up(st.cur);
                if up < st.ceiling {
                    st.cur = up; // 往上試一階（未定型每輪試；定型後只在重測輪試）
                }
            }
        }
        TARGET.set_stable_khz(st.stable); // 顯示用穩定clk
        dap.set_stable_khz(st.stable); // host 夾速(clamp)用的穩定值 = AP 確認穩的速率
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
        st.last_dp = 0; // 無 SWD 回應 → 下輪允許 probe_lines 驅動測線找斷線
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
    dap.restore_clk(saved_khz); // 還原：host 沒指定 clk 時改用穩定值當預設（否則還原 host 值）
}

