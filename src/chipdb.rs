//! 晶片/廠商/核心查表（供 OLED 顯示 layer-2 型號）。自 main.rs 抽出（R4）。
use crate::dap;

/// 查表式（`(dev_id, name)`）；涵蓋市面常見系列；GD32F1 與 STM32F1 共用 DEV_ID 故並列標示。
pub(crate) static CHIP_NAMES: &[(u16, &str)] = &[
    // F0
    (0x440, "STM32F030/05x"), (0x444, "STM32F03x"), (0x442, "STM32F09x"),
    (0x445, "STM32F04x"), (0x448, "STM32F07x"),
    // F1 / GD32F1
    (0x410, "STM32F1/GD32"), (0x412, "STM32F1 LD"), (0x414, "STM32F1/GD32 HD"),
    (0x418, "STM32F1 CL"), (0x420, "STM32F1 VL"), (0x428, "STM32F1 VL-HD"), (0x430, "STM32F1 XL"),
    // F2
    (0x411, "STM32F2"),
    // F3
    (0x422, "STM32F302/303"), (0x432, "STM32F37x"), (0x438, "STM32F334"),
    (0x439, "STM32F301/302"), (0x446, "STM32F303xE"),
    // F4
    (0x413, "STM32F405/407"), (0x419, "STM32F42x/43x"), (0x421, "STM32F446"),
    (0x423, "STM32F401xBC"), (0x431, "STM32F411"), (0x433, "STM32F401xDE"),
    (0x434, "STM32F469/479"), (0x441, "STM32F412"), (0x458, "STM32F410"), (0x463, "STM32F413"),
    // F7
    (0x449, "STM32F74x/75x"), (0x451, "STM32F76x/77x"), (0x452, "STM32F72x/73x"),
    // G0
    (0x456, "STM32G05x/06x"), (0x460, "STM32G07x/08x"), (0x466, "STM32G03x/04x"),
    (0x467, "STM32G0Bx/0Cx"),
    // G4
    (0x468, "STM32G431/441"), (0x469, "STM32G47x/48x"), (0x479, "STM32G491/4A1"),
    // L0
    (0x457, "STM32L01x/02x"), (0x425, "STM32L031/041"), (0x417, "STM32L05x/06x"),
    (0x447, "STM32L07x/08x"),
    // L1
    (0x416, "STM32L1 Cat1/2"), (0x429, "STM32L1 Cat2"), (0x427, "STM32L1 Cat3"),
    (0x436, "STM32L1 Cat4"), (0x437, "STM32L1 Cat5/6"),
    // L4 / L4+
    (0x415, "STM32L4x5/x6"), (0x435, "STM32L43x/44x"), (0x461, "STM32L496/4A6"),
    (0x462, "STM32L45x/46x"), (0x464, "STM32L41x/42x"), (0x470, "STM32L4Rx/4Sx"),
    (0x471, "STM32L4Px/4Qx"),
    // L5
    (0x472, "STM32L5"),
    // H7
    (0x450, "STM32H74x/75x"), (0x480, "STM32H7Ax/7Bx"), (0x483, "STM32H72x/73x"),
    // WB / WL
    (0x494, "STM32WB1x"), (0x495, "STM32WB55"), (0x496, "STM32WB35"), (0x497, "STM32WL5x/Ex"),
    // U5
    (0x482, "STM32U575/585"),
    // C0
    (0x443, "STM32C0"), (0x453, "STM32C0"),
];

/// JEP106 廠商碼 → 廠商名（非 ST/GD32 目標,至少顯示廠商）。查表式。
pub(crate) static VENDOR_NAMES: &[(u16, &str)] = &[
    (dap::JEP_ST, "STMicro"),
    (0x23B, "ARM"),
    (dap::JEP_NORDIC, "Nordic"),
    (dap::JEP_RASPI, "RaspberryPi"),
    (0x015, "NXP"),
    (0x00E, "NXP"),
    (0x017, "TI"),
    (0x01F, "Microchip"),
];

/// CPUID PARTNO → Cortex-M 核心名（通用辨識：任何 ARM Cortex-M 目標的後援顯示）。
pub(crate) static CORE_NAMES: &[(u16, &str)] = &[
    (0xC20, "Cortex-M0"),
    (0xC60, "Cortex-M0+"),
    (0xC21, "Cortex-M1"),
    (0xC23, "Cortex-M3"),
    (0xC24, "Cortex-M4"),
    (0xC27, "Cortex-M7"),
    (0xD20, "Cortex-M23"),
    (0xD21, "Cortex-M33"),
    (0xD22, "Cortex-M55"),
    (0xD23, "Cortex-M85"),
];

/// 在 `(key, name)` 查表中線性搜尋。
pub(crate) fn lookup(table: &[(u16, &'static str)], key: u16) -> Option<&'static str> {
    table.iter().find(|&&(k, _)| k == key).map(|&(_, name)| name)
}

pub(crate) fn core_name(part: u16) -> Option<&'static str> {
    lookup(CORE_NAMES, part)
}

pub(crate) fn chip_name(devid: u16) -> Option<&'static str> {
    lookup(CHIP_NAMES, devid)
}

pub(crate) fn vendor_name(designer: u16) -> Option<&'static str> {
    lookup(VENDOR_NAMES, designer)
}

