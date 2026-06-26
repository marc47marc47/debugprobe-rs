#!/usr/bin/env bash
# flash.sh — 建置 + 燒錄 debugprobe-rs 韌體（探針）或 layer-2 測試目標。
#
# 用法:  ./flash.sh <target>      （或  bash flash.sh <target>）
#
#   pico | rp2040     探針韌體 RP2040 (board-pico)   → elf2uf2 + picotool（需 BOOTSEL）
#   probe            探針韌體 board-debug-probe(RP2040) → 同上
#   pico2 | rp2350   探針韌體 Pico 2 (board-pico2, RP2350) → picotool convert + load
#   pico-min|probe-min  最小版（無 OLED/core1/主動偵測，純 CMSIS-DAP/USB/UART）→ 需 BOOTSEL
#   pico-diag        診斷版（插著 PC 也讓 OLED 自主偵測；只看 OLED、勿同時跑除錯工具）→ 需 BOOTSEL
#   f401             layer-2 目標 stm32f401-target（Black Pill，經探針 probe-rs SWD 燒錄）
#   f446             layer-2 目標 stm32f446-target（經探針 probe-rs SWD 燒錄）
#   f103             layer-2 目標 stm32f103-target（Blue Pill，Cortex-M3；經探針 probe-rs SWD 燒錄）
#   test-01-swdio    SWDIO/SWCLK 邊緣計數診斷版（= pico-diag，OLED 第5行 Ce/De）→ 需 BOOTSEL
#
# 環境變數:  PROBE_SERIAL=xxxx  覆蓋探針序號（預設見下）
#            SWD_SPEED=100      覆蓋 SWD 速度(kHz，預設 1000)；長線/接點差連不上時降到 100~200
#
# 註:
#  - 探針(pico/probe/pico2)用 picotool 燒,須先讓該 Pico 進 BOOTSEL
#    (拔 USB → 按住 BOOTSEL → 插回,出現 RPI-RP2 磁碟)。
#  - layer-2(f401/f446)經「探針」用 SWD 燒到目標,毋需 BOOTSEL;探針須在線、已接好 SWD。
#    若目標開讀保護(RDP)導致 probe-rs 失敗,見 MULTI-TARGET.md 的 OpenOCD unlock + flash 流程。

set -euo pipefail
cd "$(dirname "$0")"

# 探針序號：優先用環境變數 PROBE_SERIAL；否則自動偵測目前連線的 RP2040 CMSIS-DAP 探針
# （probe-rs list 第一顆 2e8a:000c）。兩顆探針互換時免得每次手動指定序號。
if [ -n "${PROBE_SERIAL:-}" ]; then
  PROBE="2e8a:000c-0:${PROBE_SERIAL}"
else
  PROBE="$(probe-rs list 2>/dev/null | grep -oE '2e8a:000c-0:[0-9A-Fa-f]+' | head -n1)" || true
  [ -z "$PROBE" ] && PROBE="2e8a:000c-0:E6604430430F8B21"   # 後備預設
fi

# SWD 連線速度（kHz）。預設 1000；杜邦線較長/接點較差的目標連不上時調低（如 PROBE 連得到但 download
# 報「Target device did not respond」就降到 100~200）。實測：某些 F103 線路 1MHz 連不上、100kHz 才穩。
SWD_SPEED="${SWD_SPEED:-1000}"

# 探針(RP2040)：建置指定 cargo 別名 → 產生 UF2 → picotool 燒。$1=alias $2=uf2檔名
flash_rp2040() {
  echo ">> cargo $1"
  cargo "$1"
  echo ">> elf2uf2-rs → target/$2"
  elf2uf2-rs target/thumbv6m-none-eabi/release/debugprobe-rs "target/$2"
  echo ">> picotool load（請確認該 Pico 已在 BOOTSEL）"
  picotool load -x "target/$2"
}

# layer-2 STM32 目標：在子 crate 建置 → 經探針 probe-rs 燒錄 + 重置。
# $1=crate $2=chip $3=target triple（預設 thumbv7em-none-eabihf；F103 等 Cortex-M3 用 thumbv7m-none-eabi）
flash_stm32() {
  local crate="$1" chip="$2" triple="${3:-thumbv7em-none-eabihf}"
  echo ">> build $crate"
  ( cd "$crate" && cargo build --release )
  local elf="$crate/target/$triple/release/$crate"
  echo ">> probe-rs download → $chip (probe $PROBE, ${SWD_SPEED}kHz)"
  probe-rs download --chip "$chip" --probe "$PROBE" --protocol swd --speed "$SWD_SPEED" "$elf"
  probe-rs reset --chip "$chip" --probe "$PROBE" --protocol swd --speed "$SWD_SPEED"
}

case "${1:-}" in
  pico | rp2040)   flash_rp2040 build-pico  debugprobe_on_pico.uf2 ;;
  probe)           flash_rp2040 build-probe debugprobe.uf2 ;;
  # 最小診斷版（無 OLED / 無 core1 / 無韌體主動偵測）→ 純 CMSIS-DAP 探針
  pico-min | rp2040-min) flash_rp2040 build-pico-min  debugprobe_on_pico_min.uf2 ;;
  probe-min)             flash_rp2040 build-probe-min debugprobe_min.uf2 ;;
  # 診斷版（插著 PC 也讓 OLED 自主偵測；只看 OLED、勿同時跑除錯工具）
  pico-diag | rp2040-diag) flash_rp2040 build-pico-diag debugprobe_on_pico_diag.uf2 ;;
  pico2 | rp2350)
    echo ">> cargo build-pico2"
    cargo build-pico2
    cp target/thumbv8m.main-none-eabihf/release/debugprobe-rs target/p2.elf
    picotool uf2 convert target/p2.elf target/debugprobe_on_pico2.uf2
    echo ">> picotool load（請確認 Pico 2 已在 BOOTSEL）"
    picotool load -x target/debugprobe_on_pico2.uf2
    ;;
  f401)            flash_stm32 stm32f401-target STM32F401CCUx ;;
  f446)            flash_stm32 stm32f446-target STM32F446RETx ;;
  f103)            flash_stm32 stm32f103-target STM32F103C8Tx thumbv7m-none-eabi ;;
  # SWDIO/SWCLK 邊緣計數診斷版（= pico-diag）：插 PC 也自主偵測，OLED 第 5 行顯示
  # 「Ce{SWCLK邊緣} De{SWDIO邊緣} {頻率}k」/「DP../AP../{速率}k」。需 BOOTSEL。
  test-01-swdio)   flash_rp2040 build-pico-diag test-01-swdio.uf2 ;;
  # 直接燒任意現成的 .uf2 檔（不重建）：./flash.sh path/to/x.uf2（需 BOOTSEL）。
  *.uf2)
    if [ ! -f "$1" ]; then
      echo "找不到檔案：$1" >&2
      exit 1
    fi
    echo ">> picotool load「$1」（請確認 Pico 已在 BOOTSEL）"
    picotool load -x "$1"
    ;;
  *)
    echo "用法: ./flash.sh {pico|rp2040|probe|pico2|rp2350|pico-min|probe-min|pico-diag|f401|f446|f103}"
    echo "  pico/rp2040  探針 RP2040 (board-pico) — 需 BOOTSEL"
    echo "  probe        探針 board-debug-probe — 需 BOOTSEL"
    echo "  pico2/rp2350 探針 Pico 2 (RP2350) — 需 BOOTSEL"
    echo "  pico-min/probe-min  最小版（無 OLED/偵測，純 CMSIS-DAP）— 需 BOOTSEL"
    echo "  pico-diag    診斷版（插 PC 也自主偵測，只看 OLED 勿跑工具）— 需 BOOTSEL"
    echo "  f401/f446    layer-2 STM32 目標（Cortex-M4，經探針 SWD 燒錄）"
    echo "  f103         layer-2 STM32 目標（Blue Pill, Cortex-M3，經探針 SWD 燒錄）"
    echo "  test-01-swdio SWDIO/SWCLK 邊緣計數診斷版（OLED 第5行 Ce/De）— 需 BOOTSEL"
    echo "  path/to/x.uf2 直接燒現成 .uf2 檔（不重建）— 需 BOOTSEL"
    echo "  PROBE_SERIAL=xxxx 覆蓋探針序號（預設 ${PROBE_SERIAL}）"
    exit 1
    ;;
esac

echo "✅ 完成。"
