//! USB iSerialNumber — 取自晶片唯一識別碼。
//! 對應 C 版 `get_serial.c`（C 用 flash unique ID，8 bytes → 16 個十六進位字元）。
//!
//! RP2040 讀 QSPI flash 的 unique ID；RP2350 改用 OTP chip id（embassy-rp 把
//! `Flash::blocking_unique_id` 限定為 rp2040）。兩者皆輸出 16 個大寫十六進位字元。

use core::fmt::Write;
use embassy_rp::Peri;
use embassy_rp::peripherals::FLASH;
use heapless::String;

/// 8 bytes → 16 個十六進位字元（與 C 的 `PICO_UNIQUE_BOARD_ID_SIZE_BYTES * 2` 一致）。
pub type SerialString = String<16>;

#[cfg(feature = "rp2040")]
pub fn read_serial(flash: Peri<'static, FLASH>) -> SerialString {
    const FLASH_SIZE: usize = 2 * 1024 * 1024;
    let mut f =
        embassy_rp::flash::Flash::<_, embassy_rp::flash::Blocking, FLASH_SIZE>::new_blocking(flash);
    let mut uid = [0u8; 8];
    // 讀失敗時退回全 0，仍可列舉。
    let _ = f.blocking_unique_id(&mut uid);
    to_hex(&uid)
}

#[cfg(feature = "rp2350")]
pub fn read_serial(_flash: Peri<'static, FLASH>) -> SerialString {
    let id = embassy_rp::otp::get_chipid().unwrap_or(0);
    to_hex(&id.to_be_bytes())
}

fn to_hex(bytes: &[u8]) -> SerialString {
    let mut s = SerialString::new();
    for b in bytes {
        // 容量足夠，忽略錯誤。
        let _ = write!(s, "{:02X}", b);
    }
    s
}
