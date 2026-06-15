/* RP2040 記憶體配置 (2 MB QSPI flash + 264 KB SRAM)。
 * embassy-rp 的 link-rp.x 會把 boot2 放進 BOOT2 區。*/
MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100
    RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}

/* picotool 可讀的 binary info（對應 C 的 bi_decl）。
 * 依 rp-binary-info 文件配置：.boot_info(含 magic+指標的標頭)必須緊接 vector table，
 * 落在 boot2 後的前 256B 內，picotool 才掃得到；.text 接在 .boot_info 之後。*/
SECTIONS {
    .boot_info : ALIGN(4)
    {
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.boot_info) + SIZEOF(.boot_info);

SECTIONS {
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;
