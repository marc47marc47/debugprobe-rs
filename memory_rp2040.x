/* RP2040 記憶體配置 (2 MB QSPI flash + 264 KB SRAM)。
 * embassy-rp 的 link-rp.x 會把 boot2 放進 BOOT2 區。*/
MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100
    RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}

/* picotool 可讀的 binary info（對應 C 的 bi_decl）。
 * .boot_info 是含 magic 與指標的標頭；.bi_entries 是項目陣列。
 * 兩者置於 .text 之後（picotool 會掃描整個映像找 magic）。*/
SECTIONS {
    .boot_info : ALIGN(4)
    {
        KEEP(*(.boot_info));
    } > FLASH
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;
