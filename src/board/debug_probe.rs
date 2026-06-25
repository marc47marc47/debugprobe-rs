//! 正式 Raspberry Pi Debug Probe 硬體 (RP2040)。
//! 對應 `debugprobe/include/board_debug_probe_config.h`。

use super::{BoardConfig, IoMode, Leds, Pins, UartPins};

pub const CONFIG: BoardConfig = BoardConfig {
    product: "Debug Probe (CMSIS-DAP)",
    io_mode: IoMode::Swdi, // SWDIO 經 level shifter，SWDI 由獨立腳讀回
    probe_sm: 0,
    pins: Pins {
        offset: 12,
        swclk: 12,
        swdio: 14,
        swdi: Some(13), // level-shifted input
        swdioen: None,
        reset: None, // 此板無 reset 腳
    },
    uart: UartPins {
        tx: 4,
        rx: 5,
        baudrate: 115_200,
        cts: None,
        rts: None,
        dtr: None,
        hwfc: false,
    },
    leds: Leds {
        usb_connected: Some(2),
        dap_connected: Some(15),
        dap_running: Some(16),
        uart_rx: Some(7),
        uart_tx: Some(8),
    },
};
