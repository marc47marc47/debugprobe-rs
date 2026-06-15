/* RP2350 記憶體配置 (Pico 2: 4 MB flash + 520 KB SRAM)。
 * RP2350 bootrom 會掃描 flash 開頭的 IMAGE_DEF 區塊；embassy-rp 會自動把
 * ImageDef 放進 .start_block section，這裡負責把這些 section 配置到 flash。*/
MEMORY {
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K
    RAM   : ORIGIN = 0x20000000, LENGTH = 520K
}

SECTIONS {
    /* RP2350 開機所需的 IMAGE_DEF / boot info 區塊，須在 vector table 之後。*/
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

/* 程式碼接在 start_block 之後。*/
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* picotool 可讀的 binary info 項目。*/
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
