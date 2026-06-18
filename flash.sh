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
#   f401             layer-2 目標 stm32f401-target（經探針 probe-rs SWD 燒錄）
#   f446             layer-2 目標 stm32f446-target（經探針 probe-rs SWD 燒錄）
#
# 環境變數:  PROBE_SERIAL=xxxx  覆蓋探針序號（預設見下）
#
# 註:
#  - 探針(pico/probe/pico2)用 picotool 燒,須先讓該 Pico 進 BOOTSEL
#    (拔 USB → 按住 BOOTSEL → 插回,出現 RPI-RP2 磁碟)。
#  - layer-2(f401/f446)經「探針」用 SWD 燒到目標,毋需 BOOTSEL;探針須在線、已接好 SWD。
#    若目標開讀保護(RDP)導致 probe-rs 失敗,見 MULTI-TARGET.md 的 OpenOCD unlock + flash 流程。

set -euo pipefail
cd "$(dirname "$0")"

PROBE_SERIAL="${PROBE_SERIAL:-E6605838834DA330}"
PROBE="2e8a:000c-0:${PROBE_SERIAL}"

# 探針(RP2040)：建置指定 cargo 別名 → 產生 UF2 → picotool 燒。$1=alias $2=uf2檔名
flash_rp2040() {
  echo ">> cargo $1"
  cargo "$1"
  echo ">> elf2uf2-rs → target/$2"
  elf2uf2-rs target/thumbv6m-none-eabi/release/debugprobe-rs "target/$2"
  echo ">> picotool load（請確認該 Pico 已在 BOOTSEL）"
  picotool load -x "target/$2"
}

# layer-2 STM32 目標：在子 crate 建置 → 經探針 probe-rs 燒錄 + 重置。$1=crate $2=chip
flash_stm32() {
  echo ">> build $1"
  ( cd "$1" && cargo build --release )
  local elf="$1/target/thumbv7em-none-eabihf/release/$1"
  echo ">> probe-rs download → $2 (probe $PROBE)"
  probe-rs download --chip "$2" --probe "$PROBE" --protocol swd --speed 1000 "$elf"
  probe-rs reset --chip "$2" --probe "$PROBE" --protocol swd
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
  *)
    echo "用法: ./flash.sh {pico|rp2040|probe|pico2|rp2350|pico-min|probe-min|pico-diag|f401|f446}"
    echo "  pico/rp2040  探針 RP2040 (board-pico) — 需 BOOTSEL"
    echo "  probe        探針 board-debug-probe — 需 BOOTSEL"
    echo "  pico2/rp2350 探針 Pico 2 (RP2350) — 需 BOOTSEL"
    echo "  pico-min/probe-min  最小版（無 OLED/偵測，純 CMSIS-DAP）— 需 BOOTSEL"
    echo "  pico-diag    診斷版（插 PC 也自主偵測，只看 OLED 勿跑工具）— 需 BOOTSEL"
    echo "  f401/f446    layer-2 STM32 目標（經探針 SWD 燒錄）"
    echo "  PROBE_SERIAL=xxxx 覆蓋探針序號（預設 ${PROBE_SERIAL}）"
    exit 1
    ;;
esac

echo "✅ 完成。"
