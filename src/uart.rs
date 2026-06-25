//! USB-CDC ↔ UART 橋接 — 對應 C 版 `cdc_uart.c`。
//!
//! 單一 async task，用 `select3` 同時等待三件事，且**只取消讀取**（cancel-safe），
//! 寫入一旦開始必然完成，避免半截寫入：
//!   1. UART RX → USB CDC IN
//!   2. USB CDC OUT → UART TX
//!   3. line coding / 控制變更（套用新 baud rate）
//!
//! 目前支援動態 baud rate 與 8N1；break / 硬體流控 / data-parity-stop 執行期切換 /
//! TX-RX LED 為後續細化（見 TODO Phase 6）。

use embassy_futures::select::{Either3, select3};
use embassy_rp::uart::BufferedUart;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use embedded_io_async::{Read, Write};

use crate::autobaud::{AutoBaud, MAGIC_BAUD};
use crate::usbdev::ProbeDriver;

/// CDC line-coding 變更後該對 UART 做什麼（取代散落的 if/else 判斷）。
enum BaudCommand {
    /// 魔術 baud（9728）→ 量測目標 RX 邊緣自動偵測。
    Auto,
    /// 一般 baud → 直接套用。
    Fixed(u32),
    /// baud=0 → 忽略。
    Ignore,
}

impl BaudCommand {
    fn from_rate(baud: u32) -> Self {
        if baud == MAGIC_BAUD {
            Self::Auto
        } else if baud > 0 {
            Self::Fixed(baud)
        } else {
            Self::Ignore
        }
    }
}

#[embassy_executor::task]
pub async fn uart_bridge_task(
    class: CdcAcmClass<'static, ProbeDriver>,
    mut uart: BufferedUart,
    mut autobaud: AutoBaud<'static>,
) {
    let (mut sender, mut receiver, control) = class.split_with_control();
    let mut ubuf = [0u8; 64];
    let mut dbuf = [0u8; 64];

    loop {
        let mut changed = false;
        {
            let (utx, urx) = uart.split_ref();
            match select3(
                urx.read(&mut ubuf),
                receiver.read_packet(&mut dbuf),
                control.control_changed(),
            )
            .await
            {
                // UART RX → USB（client log）
                Either3::First(res) => {
                    if let Ok(n) = res
                        && n > 0
                    {
                        crate::state::UART_RX_BYTES
                            .fetch_add(n as u32, core::sync::atomic::Ordering::Relaxed);
                        let _ = sender.write_packet(&ubuf[..n]).await;
                    }
                }
                // USB → UART TX
                Either3::Second(res) => {
                    if let Ok(n) = res
                        && n > 0
                    {
                        crate::state::UART_TX_BYTES
                            .fetch_add(n as u32, core::sync::atomic::Ordering::Relaxed);
                        let _ = utx.write_all(&dbuf[..n]).await;
                    }
                }
                // 控制變更（line coding / DTR / RTS）
                Either3::Third(()) => {
                    changed = true;
                }
            }
        }
        if changed {
            match BaudCommand::from_rate(receiver.line_coding().data_rate()) {
                // AutoBaud：量測目標 UART RX 的邊緣，偵測出真正的 baud 再套用。
                BaudCommand::Auto => {
                    if let Some(detected) = autobaud.detect().await {
                        uart.set_baudrate(detected);
                    }
                }
                BaudCommand::Fixed(baud) => uart.set_baudrate(baud),
                BaudCommand::Ignore => {}
            }
        }
    }
}
