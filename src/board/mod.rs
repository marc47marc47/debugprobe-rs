//! 板級設定 — 對應 C 版 `debugprobe/include/board_*.h`。
//!
//! 每個板子以 Cargo feature 選擇，匯出一組常數（腳位、LED、UART、IO 模式、
//! 產品字串等）。後續 phase 的 SWD/UART/USB 模組都以這些常數做硬體抽象。
//!
//! 多數常數會在後續 phase 才被使用，先行宣告故允許 dead_code。
#![allow(dead_code)]

/// SWDIO 的物理介面型態（對應 C 的 `PROBE_IO_RAW` / `PROBE_IO_SWDI` / `PROBE_IO_OEN`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoMode {
    /// SWCLK + SWDIO 兩線直連。
    Raw,
    /// SWCLK + SWDIO(out)，SWDI 經 level shifter 由另一支腳讀回。
    Swdi,
    /// 額外的 SWDIO output-enable (active-low) 腳。
    Oen,
}

#[cfg(feature = "board-debug-probe")]
mod debug_probe;
#[cfg(feature = "board-debug-probe")]
pub use debug_probe::*;

#[cfg(feature = "board-pico")]
mod pico;
#[cfg(feature = "board-pico")]
pub use pico::*;

#[cfg(feature = "board-pico2")]
mod pico2;
#[cfg(feature = "board-pico2")]
pub use pico2::*;
