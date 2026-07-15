# probe-rp-usb

A single-cable flash-and-debug tool for RP2040/RP2350-based devices.

When paired with firmware derived from
[embassy-rp-base](https://github.com/sunipkm/embassy-rp-base),
a single USB cable is all you need — no debug probe, no J-Link, no SWD wires.
The firmware exposes a vendor reset interface (so the host can reboot the device
into BOOTSEL mode on demand) and routes `defmt` log output over a second USB
CDC-ACM serial port.  `probe-rp-usb` drives the full workflow:

`flash new firmware` &rarr; `wait for reboot` &rarr; `stream & decode defmt logs`

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
cargo install probe-rp-usb
```

## Local Installation

```sh
cargo install --path .
```

## Linux udev setup

Regular users cannot open USB devices on Linux without a `udev` rule installed.
Install the provided rule once:

```sh
sudo cp 99-probe-rp-usb.rules /etc/udev/rules.d/
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

## macOS setup

No additional driver or permission changes are required.  `nusb` uses `IOKit`
directly, which is available to all non-sandboxed command-line tools.

If your shell reports `permission denied` when opening the serial port, make
sure your user is not excluded from the `tty` group (unusual on macOS).

## Windows setup

Serial port (`attach` / `watch` / `run`) works out of the box via the
Windows CDC-ACM class driver.

The `reset` and `flash` subcommands send vendor control transfers or PICOBOOT
commands over USB.  Windows does not have a built-in WinUSB driver for these
interfaces, so you must install one manually **once per device**:

1. Download and run [Zadig](https://zadig.akeo.ie/).
2. In Zadig, select **Options → List All Devices**.
3. Select your device (e.g. *Raspberry Pi Pico* or your custom VID).
4. Choose **WinUSB** as the driver and click **Install Driver** (or
   **Replace Driver** if another driver is already bound).

Repeat for any device with a different USB VID you intend to use.

> **Note:** The BOOTSEL mass-storage drive (used during flashing) is handled
> by the Windows USB Mass Storage driver automatically — no Zadig step needed
> for that interface.


## Usage

```
probe-rp-usb [--vid <VID>] [--pid <PID>] <SUBCOMMAND>
```

`--vid` / `--pid` accept decimal or `0x`-prefixed hex values.  When omitted,
the tool defaults to VID `0x2E8A` / PID `0x0009` (Raspberry Pi stdio USB) and
also probes VID `0xC0DE` as a fallback.

### Subcommands

#### `run` — flash and attach (one-cable equivalent of `probe-rs run`)

```sh
probe-rp-usb run target/thumbv8m.main-none-eabihf/release/my-firmware
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
probe-rp-usb flash target/thumbv8m.main-none-eabihf/release/my-firmware
```

Reboots into BOOTSEL if needed, converts the ELF to UF2, writes it, and waits
for the device to unmount before returning.

---

#### `watch` — stream defmt logs with auto-reconnect

```sh
probe-rp-usb watch target/thumbv8m.main-none-eabihf/release/my-firmware
```

Decodes `defmt` output from the USB CDC-ACM serial port.  Reconnects
automatically whenever the device resets or is reflashed.

---

#### `attach` — stream defmt logs once

```sh
probe-rp-usb attach target/thumbv8m.main-none-eabihf/release/my-firmware
```

Like `watch` but exits when the port closes instead of reconnecting.

---

#### `reset` — reboot into BOOTSEL

```sh
probe-rp-usb reset
```

Sends a vendor USB request to reboot the device into the ROM bootloader and
waits for the BOOTSEL mass-storage drive to appear.

---

#### `check` — check for a mounted BOOTSEL drive

```sh
probe-rp-usb check
```

Prints the mount path if a BOOTSEL drive is currently visible, otherwise exits
with an error.

## Companion firmware

[embassy-rp-base](https://github.com/sunipkm/embassy-rp-base) is
an Embassy-based RP2xxx firmware template that provides all the USB
interfaces `probe-rp-usb` relies on:

- **Port 0** — interactive command shell (*not required*)
- **Port 1** — `defmt` log sink (binary CDC-ACM frames, **required**)
- **Vendor reset interface** — allows `probe-rp-usb reset` / `flash` /
  `run` to reboot into BOOTSEL without the button

Derive your own firmware from that template and the whole
flash &rarr; log &rarr; reflash cycle works over the single USB cable
without needing to press the BOOTSEL button or resetting
the system.
