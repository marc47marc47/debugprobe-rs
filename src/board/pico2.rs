//! Raspberry Pi Pico 2 (RP2350) 當作 Debug Probe。
//! C 版以 `board_pico_config.h` + `PICO_BOARD=pico2` 共用設定，腳位與 Pico 1 相同。

use super::{BoardConfig, IoMode, Leds, Pins, UartPins};

pub const CONFIG: BoardConfig = BoardConfig {
    product: "Debugprobe on Pico 2 (CMSIS-DAP)",
    io_mode: IoMode::Raw, // SWCLK/SWDIO 兩線直連
    probe_sm: 0,
    pins: Pins {
        offset: 2,
        swclk: 2,
        swdio: 3,
        swdi: None,
        swdioen: None,
        reset: Some(1), // 目標 reset (active-low)
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
        usb_connected: Some(25), // 板載 LED 在 GPIO25
        dap_connected: None,
        dap_running: None,
        uart_rx: None,
        uart_tx: None,
    },
};
