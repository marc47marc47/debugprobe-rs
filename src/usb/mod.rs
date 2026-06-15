//! USB 裝置設定 — 對應 C 版 `usb_descriptors.c` + `tusb_edpt_handler.c`。
//!
//! 建立 device/字串描述符、BOS + MS OS 2.0（WinUSB driverless），以及
//! CMSIS-DAP v2 的 vendor interface（bulk IN/OUT）。CDC-ACM（UART 橋接）於 Phase 6。

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::msos::{self, windows_version};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::StaticCell;

use crate::board;

/// 此專案使用的 USB driver 具體型別。
pub type ProbeDriver = Driver<'static, USB>;

/// DAP bulk endpoint 型別。
pub type DapEpOut = <ProbeDriver as UsbDriverTrait<'static>>::EndpointOut;
pub type DapEpIn = <ProbeDriver as UsbDriverTrait<'static>>::EndpointIn;

/// CMSIS-DAP v2 的 bulk 傳輸端點（對應 C 的 OUT/IN endpoint）。
pub struct DapTransport {
    pub read_ep: DapEpOut,
    pub write_ep: DapEpIn,
}

/// WinUSB DeviceInterfaceGUID（對應 C `usb_descriptors.c` 的 desc_ms_os_20）。
const DEVICE_INTERFACE_GUID: &str = "{CDB3B5AD-293B-4663-AA36-1AAE46463776}";
/// MS OS 2.0 vendor request code（對應 C `tud_vendor_control_xfer_cb` 的 bRequest == 1）。
pub const MSOS_VENDOR_CODE: u8 = 0x01;

/// embassy-usb Builder 需要的 'static 緩衝區。
struct UsbBuffers {
    config_descriptor: [u8; 256],
    bos_descriptor: [u8; 256],
    msos_descriptor: [u8; 256],
    control_buf: [u8; 64],
}

/// 建立 USB 裝置、DAP 傳輸端點與 CDC-ACM 類別。
/// 呼叫端負責 spawn `device.run()`、DAP task 與 UART 橋接 task。
pub fn build(
    driver: ProbeDriver,
    serial: &'static str,
) -> (
    UsbDevice<'static, ProbeDriver>,
    DapTransport,
    CdcAcmClass<'static, ProbeDriver>,
) {
    // --- Device descriptor（對應 C desc_device）---
    let mut config = Config::new(0x2e8a, 0x000c); // VID Raspberry Pi / PID CMSIS-DAP
    config.manufacturer = Some("Raspberry Pi");
    config.product = Some(board::PRODUCT_STRING);
    config.serial_number = Some(serial);
    config.device_release = 0x0231; // bcdDevice 02.31
    config.max_packet_size_0 = 64;
    // 複合裝置（DAP vendor + CDC）需 IAD；embassy 要求對應的
    // Miscellaneous/Common/IAD 裝置類別 0xEF/0x02/0x01。
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    static BUFS: StaticCell<UsbBuffers> = StaticCell::new();
    let bufs = BUFS.init(UsbBuffers {
        config_descriptor: [0; 256],
        bos_descriptor: [0; 256],
        msos_descriptor: [0; 256],
        control_buf: [0; 64],
    });

    let mut builder = Builder::new(
        driver,
        config,
        &mut bufs.config_descriptor,
        &mut bufs.bos_descriptor,
        &mut bufs.msos_descriptor,
        &mut bufs.control_buf,
    );

    // --- BOS + MS OS 2.0 表頭（WinUSB driverless）---
    builder.msos_descriptor(windows_version::WIN8_1, MSOS_VENDOR_CODE);

    // --- CMSIS-DAP v2：vendor interface (0xFF) + bulk IN/OUT ---
    let (read_ep, write_ep) = {
        let mut func = builder.function(0xFF, 0x00, 0x00);
        // function-level WinUSB 綁定（對應 C 的 MS OS 2.0 function subset）
        func.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
        func.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
            "DeviceInterfaceGUIDs",
            msos::PropertyData::RegMultiSz(&[DEVICE_INTERFACE_GUID]),
        ));
        let mut iface = func.interface();
        let mut alt = iface.alt_setting(0xFF, 0x00, 0x00, None);
        let read_ep = alt.endpoint_bulk_out(None, 64);
        let write_ep = alt.endpoint_bulk_in(None, 64);
        (read_ep, write_ep)
    };

    // --- CDC-ACM（UART 橋接，對應 C 的介面 1+2）---
    static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
    let cdc = CdcAcmClass::new(&mut builder, CDC_STATE.init(CdcState::new()), 64);

    let device = builder.build();
    (device, DapTransport { read_ep, write_ep }, cdc)
}
