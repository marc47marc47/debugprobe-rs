//! Raspberry Pi Pico 2 (RP2350) 當作 Debug Probe。
//! C 版以 `board_pico_config.h` + `PICO_BOARD=pico2` 共用設定，腳位與 Pico 1 相同。

use super::IoMode;

pub const PRODUCT_STRING: &str = "Debugprobe on Pico 2 (CMSIS-DAP)";

/// SWCLK/SWDIO 兩線直連。
pub const IO_MODE: IoMode = IoMode::Raw;

// --- SWD PIO 腳位 (PIN_OFFSET = 2) ---
pub const PROBE_SM: usize = 0;
pub const PIN_OFFSET: u8 = 2;
pub const PIN_SWCLK: u8 = PIN_OFFSET; // 2
pub const PIN_SWDIO: u8 = PIN_OFFSET + 1; // 3
pub const PIN_SWDI: Option<u8> = None;
pub const PIN_SWDIOEN: Option<u8> = None;
pub const PIN_RESET: Option<u8> = Some(1); // 目標 reset (active-low)

// --- UART (目標橋接，UART1) ---
pub const UART_TX: u8 = 4;
pub const UART_RX: u8 = 5;
pub const UART_BAUDRATE: u32 = 115_200;
pub const UART_CTS: Option<u8> = None;
pub const UART_RTS: Option<u8> = None;
pub const UART_DTR: Option<u8> = None;
pub const UART_HWFC: bool = false;

// --- LED (板載 LED 在 GPIO25) ---
pub const LED_USB_CONNECTED: Option<u8> = Some(25);
pub const LED_DAP_CONNECTED: Option<u8> = None;
pub const LED_DAP_RUNNING: Option<u8> = None;
pub const LED_UART_RX: Option<u8> = None;
pub const LED_UART_TX: Option<u8> = None;
