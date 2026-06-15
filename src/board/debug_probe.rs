//! 正式 Raspberry Pi Debug Probe 硬體 (RP2040)。
//! 對應 `debugprobe/include/board_debug_probe_config.h`。

use super::IoMode;

pub const PRODUCT_STRING: &str = "Debug Probe (CMSIS-DAP)";

/// SWDIO 經 level shifter，SWDI 由獨立腳讀回。
pub const IO_MODE: IoMode = IoMode::Swdi;

// --- SWD PIO 腳位 (PIN_OFFSET = 12) ---
pub const PROBE_SM: usize = 0;
pub const PIN_OFFSET: u8 = 12;
pub const PIN_SWCLK: u8 = PIN_OFFSET; // 12
pub const PIN_SWDI: Option<u8> = Some(PIN_OFFSET + 1); // 13 (level-shifted input)
pub const PIN_SWDIO: u8 = PIN_OFFSET + 2; // 14
pub const PIN_SWDIOEN: Option<u8> = None;
pub const PIN_RESET: Option<u8> = None; // 此板無 reset 腳

// --- UART (目標橋接，UART1) ---
pub const UART_TX: u8 = 4;
pub const UART_RX: u8 = 5;
pub const UART_BAUDRATE: u32 = 115_200;
pub const UART_CTS: Option<u8> = None;
pub const UART_RTS: Option<u8> = None;
pub const UART_DTR: Option<u8> = None;
pub const UART_HWFC: bool = false;

// --- LED ---
pub const LED_USB_CONNECTED: Option<u8> = Some(2);
pub const LED_DAP_CONNECTED: Option<u8> = Some(15);
pub const LED_DAP_RUNNING: Option<u8> = Some(16);
pub const LED_UART_RX: Option<u8> = Some(7);
pub const LED_UART_TX: Option<u8> = Some(8);
