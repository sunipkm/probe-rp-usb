# chickadee-probe

A single-cable flash-and-debug tool for RP2040/RP2350-based devices.

When paired with firmware derived from
[chickadee-rp2350-base](https://github.com/sunipkm/chickadee-rp2350-base),
a single USB cable is all you need — no debug probe, no J-Link, no SWD wires.
The firmware exposes a vendor reset interface (so the host can reboot the device
into BOOTSEL mode on demand) and routes `defmt` log output over a second USB
CDC-ACM serial port.  `chickadee-probe` drives the full workflow:

```
flash new firmware  →  wait for reboot  →  stream & decode defmt logs
```

all from one command, over the same USB cable that powers the board.

## Features

- Reboot into BOOTSEL mode without touching the button
- Convert ELF or raw binary to UF2 and flash via the BOOTSEL mass-storage
  interface
- Decode `defmt` output streamed over USB CDC-ACM (no probe hardware needed)
- Watch mode: reconnect automatically across device resets and reflashes
- `run` subcommand: flash + watch in a single invocation (equivalent to
  `probe-rs run`, but over USB only)

## Installation

```sh
cargo install --path .
```

## Linux udev setup

Without a udev rule regular users cannot open USB devices.  Install the
provided rule once:

```sh
sudo cp 99-chickadee-probe.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Your user must be in the `plugdev` group (`groups $USER`).  If not:

```sh
sudo usermod -aG plugdev $USER   # log out and back in to apply
```

The rule file covers:

| VID    | Description                                |
|--------|--------------------------------------------|
| `2E8A` | Raspberry Pi (app mode + BOOTSEL/picoboot) |
| `C0DE` | Custom VID fallback                        |

## Usage

```
chickadee-probe [--vid <VID>] [--pid <PID>] <SUBCOMMAND>
```

`--vid` / `--pid` accept decimal or `0x`-prefixed hex values.  When omitted,
the tool defaults to VID `0x2E8A` / PID `0x0009` (Raspberry Pi stdio USB) and
also probes VID `0xC0DE` as a fallback.

### Subcommands

#### `run` — flash and attach (one-cable equivalent of `probe-rs run`)

```sh
chickadee-probe run target/thumbv8m.main-none-eabihf/release/my-firmware
```

Flashes the ELF, waits for the device to reboot, then streams and decodes
`defmt` output in watch mode.  Reconnects automatically if the device resets.

Options:

| Flag | Default | Description |
|------|---------|-------------|
| `--family` | `rp2350-arm-s` | UF2 family tag (`rp2040`, `rp2350-arm-s`, `rp2350-arm-ns`, `rp2350-riscv`) |
| `--address` | `0x10000000` | Flash base address (raw binary inputs only) |
| `--port` | auto-detect | Override the serial port |

---

#### `flash` — write firmware only

```sh
chickadee-probe flash target/thumbv8m.main-none-eabihf/release/my-firmware
```

Reboots into BOOTSEL if needed, converts the ELF to UF2, writes it, and waits
for the device to unmount before returning.

---

#### `watch` — stream defmt logs with auto-reconnect

```sh
chickadee-probe watch target/thumbv8m.main-none-eabihf/release/my-firmware
```

Decodes `defmt` output from the USB CDC-ACM serial port.  Reconnects
automatically whenever the device resets or is reflashed.

---

#### `attach` — stream defmt logs once

```sh
chickadee-probe attach target/thumbv8m.main-none-eabihf/release/my-firmware
```

Like `watch` but exits when the port closes instead of reconnecting.

---

#### `reset` — reboot into BOOTSEL

```sh
chickadee-probe reset
```

Sends a vendor USB request to reboot the device into the ROM bootloader and
waits for the BOOTSEL mass-storage drive to appear.

---

#### `check` — check for a mounted BOOTSEL drive

```sh
chickadee-probe check
```

Prints the mount path if a BOOTSEL drive is currently visible, otherwise exits
with an error.

## Companion firmware

[chickadee-rp2350-base](https://github.com/sunipkm/chickadee-rp2350-base) is
an Embassy-based RP235xA/B firmware template that provides all the USB
interfaces `chickadee-probe` relies on:

- **Port 0** — interactive command shell
- **Port 1** — `defmt` log sink (binary CDC-ACM frames)
- **Vendor reset interface** — allows `chickadee-probe reset` / `flash` /
  `run` to reboot into BOOTSEL without the button

Derive your own firmware from that template and the whole
flash → log → reflash cycle works over the single USB cable.
