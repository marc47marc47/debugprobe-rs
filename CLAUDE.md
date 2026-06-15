# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`debugprobe-rs` — the Raspberry Pi Debug Probe firmware rewritten from C (FreeRTOS + TinyUSB +
Pico SDK) to **Rust + Embassy**, targeting RP2040 (Debug Probe / Pico) and RP2350 (Pico 2).
It is a CMSIS-DAP SWD debug probe + USB-to-UART bridge. The original C firmware is kept as a
read-only reference in `debugprobe/` (a separate upstream git repo, gitignored) with its own
architecture write-up in `debugprobe/DEVELOP.md`. Progress is tracked phase-by-phase in `TODO.md`;
performance limits are in `STRESS-test.md`.

## Build / flash / test commands

One Cargo package, selected per board via mutually-exclusive features. **`cargo build` defaults to
`thumbv6m-none-eabi`** (`.cargo/config.toml`). Aliases:

```bash
cargo build-probe    # board-debug-probe (RP2040)  — default board feature
cargo build-pico     # board-pico (RP2040, Pico 1)
cargo build-pico2    # board-pico2 (RP2350; adds --target thumbv8m.main-none-eabihf)
cargo run-probe      # build + flash via probe-rs (SWD programmer) + RTT
cargo clippy --no-default-features --features board-pico        # lint rp2040
cargo clippy --target thumbv8m.main-none-eabihf --no-default-features --features board-pico2
```

UF2 generation differs by chip:
```bash
# RP2040: elf2uf2-rs
elf2uf2-rs target/thumbv6m-none-eabi/release/debugprobe-rs target/debugprobe.uf2
# RP2350: picotool (needs .elf extension); elf2uf2-rs picks the wrong family for rp2350
cp target/thumbv8m.main-none-eabihf/release/debugprobe-rs target/p2.elf
picotool uf2 convert target/p2.elf target/debugprobe_on_pico2.uf2
```

Flashing the probe Pico (layer 1) needs **BOOTSEL** (physical button) → RPI-RP2 drive (copy UF2) or
`picotool load -x file.uf2`. There is no software path to BOOTSEL the running probe.

Hardware test targets live in `src/bin/` and are flashed onto a **target** RP2040 *through* a working
probe via probe-rs (no BOOTSEL needed on the target):
```bash
cargo build --release --no-default-features --features rp2040 --bin uartecho   # also uarthello / uartmon
probe-rs download --chip RP2040 --probe "2e8a:000c-0:<serial>" --protocol swd --speed 1000 \
  target/thumbv6m-none-eabi/release/uartecho
probe-rs reset    --chip RP2040 --probe "2e8a:000c-0:<serial>" --protocol swd
```
Functional verification is done on hardware with `probe-rs info`, OpenOCD
(`-f interface/cmsis-dap.cfg -f target/rp2040.cfg`), and pyOCD. There are no host unit tests
(it's `no_std` firmware).

## Architecture (big picture)

C/FreeRTOS tasks → Embassy `#[embassy_executor::task]` async tasks. The DAP command pipeline is a
single chain that must be understood across files:

**USB transport (`src/usb/mod.rs`) → DAP core (`src/dap/mod.rs`) → SWD physical layer
(`src/probe/mod.rs`, PIO).**

- `src/main.rs` — entry, `bind_interrupts!`, spawns tasks, cross-task state (atomics). `dap_task`
  `select`s over **both** the CMSIS-DAP v2 bulk OUT endpoint and the v1 HID OUT report and feeds the
  same `Dap` core, so probe-rs/OpenOCD (v2) and pyOCD (v1 HID) work from one build.
- `src/usb/mod.rs` — device/descriptors, BOS + MS OS 2.0 (WinUSB driverless), the DAP vendor (v2 bulk)
  interface, the v1 HID interface, and CDC-ACM. Returns endpoint/class handles to `main`.
- `src/dap/mod.rs` — CMSIS-DAP command parser rewritten from scratch (SWD only; JTAG/SWO off) plus the
  SWD transfer logic (request/ACK/parity/WAIT-retry/posted-AP-read), `async` because it drives PIO.
- `src/probe/mod.rs` — SWD over PIO0 SM0 via `pio::pio_asm!`. The PIO program is **reordered vs the C
  version** so `get_next_cmd` is at the program origin (embassy `set_config` jumps to origin on start).
  Command word format `| 13:9 cmd | 8 dir | 7:0 count |`; absolute jump targets = `loaded.origin +
  public_define`. SWCLK = `clk_sys / (4 × divider)`; divider is integer (so requested kHz snaps to the
  nearest achievable rate).
- `src/uart.rs` — CDC-ACM ↔ UART1 bridge; `select3` cancels only reads (cancel-safe), writes always
  complete. Dynamic baud via CDC line coding; magic baud 9728 triggers AutoBaud.
- `src/autobaud.rs` — measures UART RX edge timing on **PIO1** to detect baud. Key trick: sets the SM's
  `in_base`/`jmp_pin` to GP5 via `embassy_rp::pac` (needs `unstable-pac` feature) **without**
  `make_pio_pin`, so UART1 keeps owning the pin and PIO1 just reads its input synchronizer.
- `src/board/{debug_probe,pico,pico2}.rs` — per-board pins/LED/UART/IO-mode/product-string constants
  (mirror `debugprobe/include/board_*.h`), selected by the `board-*` feature in `src/board/mod.rs`.
- `src/display.rs` — optional SSD1306 OLED status (I2C1 GP6/GP7), BufferedGraphicsMode. Silently no-ops
  if the panel is absent. `serial.rs` reads the USB serial from flash unique ID (rp2040) / OTP (rp2350).

## Non-obvious gotchas (cost real debugging time — keep these in mind)

- **Multicore: core0 `main` must NOT return.** OLED+LED run on core1 (`spawn_core1`, 32 KB stack);
  core0 runs USB/DAP/UART/AutoBaud. If `#[embassy_executor::main]` returns after `spawn_core1`, DAP
  **AP access fails consistently** (DP IDCODE still reads). End `main` with `core::future::pending().await`.
- **Board build-path collision:** every rp2040 board feature builds the same default bin at
  `target/thumbv6m-none-eabi/release/debugprobe-rs`. Always rebuild the intended board *immediately*
  before generating its UF2, or you'll flash the wrong variant (different SWD pins).
- **OLED I2C is blocking** (~10 ms/flush) and stalls its executor. Putting it on core1 (multicore)
  isolates it from core0's DAP; the single-core fallback skips the flush during DAP activity.
- **`embassy_rp::pac` requires the `unstable-pac` feature** (used by AutoBaud).
- **RP2040 vs RP2350 linking:** `build.rs` selects `memory_rp2040.x` / `memory_rp2350.x` by chip
  feature. RP2040 passes `-Tlink-rp.x` (embassy-rp emits it, rp2040 only); RP2350 must NOT
  (`.cargo/config.toml` per-target rustflags). boot2 (rp2040) / image-def (rp2350) are auto-provided
  by embassy-rp. For picotool to show binary_info on rp2040, `.boot_info` must be placed right after
  `.vector_table` (within the first 256 B after boot2) — see `memory_rp2040.x`.
- **`embassy-executor` 0.10:** the `#[task]` fn returns `Result<SpawnToken,_>`; spawn with
  `spawner.spawn(task(...).unwrap())`. The arch feature is `platform-cortex-m` (not `arch-cortex-m`).
- **Atomics on thumbv6m:** use `portable_atomic` (RMW like `fetch_add` isn't in `core` on M0+).
- **USB composite (DAP + CDC + HID) needs IAD:** `composite_with_iads = true` requires device class
  `0xEF/0x02/0x01`, else `Builder::new` panics and the device never enumerates.

## Conventions

- Comments and commit messages are in 繁體中文 (matching existing history). End commit messages with the
  `Co-Authored-By` trailer used in prior commits.
- The repo intentionally gitignores `debugprobe/` (C reference clone), `*.pdf`, `*.ps1`, `comout.txt`,
  `/target`.
