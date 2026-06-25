//! 自主掃描：adaptive_sweep + idle_scan。自 main.rs 抽出（Phase 13 R5）。
use crate::state::{TARGET, WAVE};
use crate::wiring::classify;
use crate::{dap, logic};
use embassy_time::{Duration, Timer};

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
    sticky: &mut u32,
    absent: &mut u32,
    prev: &mut Option<(bool, bool)>,
) {
    use embassy_futures::select::select;
    let saved_khz = dap.swclk_khz();

    // 走線監測第一步：逐線連通（drive 反向→釋放→讀目標內部 pull）。判斷哪條線實體斷掉。
    let (dio, clk) = dap.probe_lines().await;
    TARGET.set_lines(dio, clk);
    // 鬆動統計：與上輪比較，連通狀態翻轉即累加（反覆拔插時，flap 最多者 = 最會鬆的線）。
    if let Some((pd, pc)) = *prev {
        if dio != pd {
            TARGET.bump_dio_flap();
        }
        if clk != pc {
            TARGET.bump_clk_flap();
        }
    }
    *prev = Some((dio, clk));

    let used = adaptive_sweep(dap, sticky).await;
    TARGET.set_used_khz(used);

    let mut buf = [0u32; logic::CAP_WORDS];
    let ce;
    let (dp, ap);
    if used != 0 {
        *absent = 0;
        // 偵測到目標(可用速率)才擷取波形 + 量邊緣(此時 SWCLK 在跑、量得到真訊號)。
        cap.start();
        let xfer = cap.dma_into(&mut buf);
        let _ = dap.swd_read_dpidr().await; // 訊號刺激
        let _ = select(xfer, Timer::after(Duration::from_millis(20))).await;
        cap.stop();
        let (ce0, de, ch, dh) = logic::count_signal(&buf);
        TARGET.set_signal(ce0, de, ch, dh);
        WAVE.push(&buf);
        // 晶片偵測只在尚未鎖定時做一次（F2 功能保留）。
        if !TARGET.valid()
            && let Some(info) = dap.detect_target().await
        {
            TARGET.store(&info);
        }
        let q = dap.link_quality().await;
        TARGET.set_link(&q);
        ce = ce0;
        dp = q.dp as u32;
        ap = q.ap as u32;
    } else {
        // 無目標：不擷取(低速擷取無意義)、推平線、歸零。
        WAVE.push_flat();
        TARGET.set_signal(0, 0, 0, 0);
        TARGET.set_link(&dap::LinkQuality { dp: 0, ap: 0 });
        *absent += 1;
        if *absent >= 2 {
            TARGET.clear();
        }
        ce = 0;
        dp = 0;
        ap = 0;
    }
    // 走線判定：彙整逐線連通 + 連線品質 → 結論（供 OLED 即時顯示哪條線壞）。
    // captured = 本輪是否真的擷取了波形（used!=0）；只有此時 ce 才有意義。
    TARGET.set_verdict(classify(dio, clk, used != 0, ce, dp, ap));
    dap.set_swclk_khz(saved_khz); // 還原 host 設定
}

