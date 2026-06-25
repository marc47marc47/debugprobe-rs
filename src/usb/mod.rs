//! USB 裝置設定 — 對應 C 版 `usb_descriptors.c` + `tusb_edpt_handler.c`。
//!
//! 建立 device/字串描述符、BOS + MS OS 2.0（WinUSB driverless），以及
//! CMSIS-DAP v2 的 vendor interface（bulk IN/OUT）。CDC-ACM（UART 橋接）於 Phase 6。

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::class::hid::{
    Config as HidConfig, HidBootProtocol, HidReader, HidReaderWriter, HidSubclass, HidWriter,
    State as HidState,
};
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

/// CMSIS-DAP 傳輸：v2 bulk（probe-rs/OpenOCD）+ v1 HID（pyOCD/legacy）同時提供。
pub struct DapTransport {
    pub read_ep: DapEpOut,
    pub write_ep: DapEpIn,
    pub hid_reader: HidReader<'static, ProbeDriver, 64>,
    pub hid_writer: HidWriter<'static, ProbeDriver, 64>,
}

/// CMSIS-DAP v1 HID report descriptor（vendor I/O，64-byte in/out）。
const DAP_HID_REPORT_DESC: &[u8] = &[
    0x06, 0x00, 0xFF, // Usage Page (Vendor 0xFF00)
    0x09, 0x01, // Usage 1
    0xA1, 0x01, // Collection (Application)
    0x15, 0x00, //   Logical Minimum 0
    0x26, 0xFF, 0x00, //   Logical Maximum 255
    0x75, 0x08, //   Report Size 8
    0x95, 0x40, //   Report Count 64
    0x09, 0x01, //   Usage 1
    0x81, 0x02, //   Input (Data,Var,Abs)
    0x09, 0x01, //   Usage 1
    0x91, 0x02, //   Output (Data,Var,Abs)
    0xC0, // End Collection
];

/// WinUSB DeviceInterfaceGUID（對應 C `usb_descriptors.c` 的 desc_ms_os_20）。
const DEVICE_INTERFACE_GUID: &str = "{CDB3B5AD-293B-4663-AA36-1AAE46463776}";
/// MS OS 2.0 vendor request code（對應 C `tud_vendor_control_xfer_cb` 的 bRequest == 1）。
pub const MSOS_VENDOR_CODE: u8 = 0x01;

/// USB 裝置識別常數（集中散落的魔術數）。
const USB_VID: u16 = 0x2e8a; // Raspberry Pi
const USB_PID: u16 = 0x000c; // CMSIS-DAP
const USB_MANUFACTURER: &str = "Raspberry Pi";
const USB_DEVICE_RELEASE: u16 = 0x0231; // bcdDevice 02.31
const USB_EP0_SIZE: u8 = 64;

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
    let mut config = Config::new(USB_VID, USB_PID);
    config.manufacturer = Some(USB_MANUFACTURER);
    config.product = Some(board::PRODUCT_STRING);
    config.serial_number = Some(serial);
    config.device_release = USB_DEVICE_RELEASE;
    config.max_packet_size_0 = USB_EP0_SIZE;
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

    // --- CMSIS-DAP v1 HID（pyOCD / legacy HID 工具）---
    static HID_STATE: StaticCell<HidState> = StaticCell::new();
    let hid_cfg = HidConfig {
        report_descriptor: DAP_HID_REPORT_DESC,
        request_handler: None,
        poll_ms: 1,
        max_packet_size: 64,
        hid_subclass: HidSubclass::No,
        hid_boot_protocol: HidBootProtocol::None,
    };
    let hid = HidReaderWriter::<_, 64, 64>::new(&mut builder, HID_STATE.init(HidState::new()), hid_cfg);
    let (hid_reader, hid_writer) = hid.split();

    let device = builder.build();
    (
        device,
        DapTransport {
            read_ep,
            write_ep,
            hid_reader,
            hid_writer,
        },
        cdc,
    )
}
