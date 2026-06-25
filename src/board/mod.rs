//! 板級設定 — 對應 C 版 `debugprobe/include/board_*.h`。
//!
//! 每個板子以 Cargo feature 選擇，匯出單一 [`CONFIG`]（[`BoardConfig`]）。SWD/UART/USB
//! 等模組都以 `board::CONFIG.*` 做硬體抽象。改用 struct（取代散落的裸 const）後，
//! 漏填任一欄位會編譯錯，編譯期保證三板設定齊全。
//!
//! 多數欄位會在後續 phase 才被使用，故允許 dead_code。
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

/// SWD PIO 腳位。
pub struct Pins {
    pub offset: u8,
    pub swclk: u8,
    pub swdio: u8,
    /// level-shifter 讀回腳（Swdi 模式）。
    pub swdi: Option<u8>,
    /// SWDIO output-enable 腳（Oen 模式）。
    pub swdioen: Option<u8>,
    /// 目標 reset（active-low）。
    pub reset: Option<u8>,
}

/// UART 橋接腳位/設定（UART1）。
pub struct UartPins {
    pub tx: u8,
    pub rx: u8,
    pub baudrate: u32,
    pub cts: Option<u8>,
    pub rts: Option<u8>,
    pub dtr: Option<u8>,
    pub hwfc: bool,
}

/// 狀態 LED（None = 該板無此 LED）。
pub struct Leds {
    pub usb_connected: Option<u8>,
    pub dap_connected: Option<u8>,
    pub dap_running: Option<u8>,
    pub uart_rx: Option<u8>,
    pub uart_tx: Option<u8>,
}

/// 一塊板子的完整硬體設定。
pub struct BoardConfig {
    pub product: &'static str,
    pub io_mode: IoMode,
    pub probe_sm: usize,
    pub pins: Pins,
    pub uart: UartPins,
    pub leds: Leds,
}

#[cfg(feature = "board-debug-probe")]
mod debug_probe;
#[cfg(feature = "board-debug-probe")]
pub use debug_probe::CONFIG;

#[cfg(feature = "board-pico")]
mod pico;
#[cfg(feature = "board-pico")]
pub use pico::CONFIG;

#[cfg(feature = "board-pico2")]
mod pico2;
#[cfg(feature = "board-pico2")]
pub use pico2::CONFIG;
