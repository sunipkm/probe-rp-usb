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
- Flash ELF or raw binary images through the BOOTSEL PICOBOOT vendor USB
  interface by default, with UF2 mass-storage available as a compatibility
  backend
- Read an exact byte range from flash into a file
- Decode `defmt` output streamed over USB CDC-ACM (no probe hardware needed)
- Watch mode: reconnect automatically across device resets and reflashes
- `run` subcommand: flash + watch in a single invocation (equivalent to
  `probe-rs run`, but over USB only)

## Installation

### Using the installer script (pre-built binary)

**Linux / macOS:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.sh \
  | sh
```

Or with `wget`:

```sh
wget -qO- \
  https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.sh \
  | sh
```

Options can be passed via environment variables or flags:

| Option | Description |
|--------|-------------|
| `--version <tag>` | Install a specific release tag (e.g. `v0.2.0`) |
| `--no-modify-path` | Skip adding the binary to `PATH` |
| `--verbose` / `--quiet` | Control output verbosity |

Environment overrides: `PROBE_RP_USB_VERSION`, `PROBE_RP_USB_INSTALL_DIR`, `INSTALLER_NO_MODIFY_PATH`.

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.ps1 | iex"
```

To install a specific version or a custom directory:

```powershell
.\probe-rp-usb-installer.ps1 -Version "v0.2.0" -InstallDir "$env:ProgramFiles\probe-rp-usb"
```

### Using cargo

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

Your user must be in the `plugdev` group for raw USB access and `dialout` for
serial ports (`groups $USER`).  If not:

```sh
sudo usermod -aG plugdev $USER   # raw USB device access
sudo usermod -aG dialout $USER   # serial port access
# log out and back in to apply group changes
```

The rule file covers:

| VID    | Description                                |
|--------|--------------------------------------------|
| `2E8A` | Raspberry Pi (app mode + BOOTSEL/picoboot) |
| `C0DE` | Custom VID fallback                        |
| `C001` | Custom VID fallback                        |

## macOS setup

No additional driver or permission changes are required.  `nusb` uses `IOKit`
directly, which is available to all non-sandboxed command-line tools.

If your shell reports `permission denied` when opening the serial port, make
sure your user is not excluded from the `tty` group (unusual on macOS).

## Windows setup

Serial port (`attach` / `watch` / `run`) works out of the box via the
Windows CDC-ACM class driver.

The `reset` and `flash` subcommands send vendor control transfers or PICOBOOT
commands over USB. On Windows, `probe-rp-usb` now attempts to install a
WinUSB binding automatically when it detects a matching reset or PICOBOOT
interface that has no suitable driver yet.

If Windows prompts for elevation, rerun from an Administrator shell.

Automatic driver setup does not replace an already-bound non-WinUSB driver.
If a required interface is already claimed by another driver, replace it
manually with [Zadig](https://zadig.akeo.ie/):

1. Open Zadig and enable **Options → List All Devices**.
2. Select the vendor reset or PICOBOOT interface for your device.
3. Choose **WinUSB** and click **Install Driver** or **Replace Driver**.

The BOOTSEL mass-storage binding (`USBSTOR`) is left alone intentionally.


## Usage

```
probe-rp-usb [--vid <VID>] [--pid <PID>] <SUBCOMMAND>
```

`--vid` / `--pid` accept decimal or `0x`-prefixed hex values.  When omitted,
the tool defaults to VID `0x2E8A` / PID `0x0009` (Raspberry Pi stdio USB) and
also probes VID `0xC0DE` and `0xC001` as fallbacks.

ELF inputs are converted with the local `probe_rp_usb::elf` module. Raw binary
inputs are wrapped directly as UF2 pages or written with PICOBOOT at the address
you provide.

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
| `--family` | `rp2350-arm-s` | UF2 family tag (`rp2040`, `rp2xxx-absolute`, `rp2xxx-data`, `rp2350-arm-s`, `rp2350-arm-ns`, `rp2350-riscv`) |
| `--address` | `0x10000000` | Flash base address (raw binary inputs only) |
| `--port` | auto-detect | Override the serial port |

---

#### `flash` — write firmware only

```sh
probe-rp-usb flash target/thumbv8m.main-none-eabihf/release/my-firmware
```

Reboots into BOOTSEL if needed, writes the image through the PICOBOOT vendor
USB interface, and then reboots the device. ELF inputs are converted to UF2 in
memory to determine their flash layout, then written directly over PICOBOOT.
Raw binary inputs are written at `--address`.

Use `--backend uf2` to use the BOOTSEL mass-storage drive instead.

Options:

| Flag | Default | Description |
|------|---------|-------------|
| `--backend` | `picoboot` | `picoboot` for direct USB commands, `uf2` for mass-storage flashing |
| `--family` | `rp2350-arm-s` | UF2 family tag used for ELF layout or UF2 output |
| `--address` | `0x10000000` | Flash base address (raw binary inputs only) |
| `--no-wait` | disabled | Leave the device in BOOTSEL mode after flashing |

---

#### `write` — write raw data ranges

```sh
probe-rp-usb write settings.bin@0x10040000 assets.bin@0x10100000
```

Writes one or more raw binary files at exact flash addresses. The PICOBOOT
backend performs sector read-modify-erase-write, so unaligned writes preserve
neighboring bytes in the same flash sector.

Use `--base` to add a common base address to every `FILE@OFFSET`, and
`--backend uf2` to use the mass-storage fallback.

---

#### `read` — read a flash byte range

```sh
probe-rp-usb read 0x10000000 0x10000 firmware.bin
```

Reboots into BOOTSEL if needed and reads exactly `LENGTH` bytes starting at
`ADDRESS` into `OUTPUT` using the PICOBOOT vendor USB interface.

---

#### `erase` — erase a flash range

```sh
probe-rp-usb erase 0x400000
```

Erases `FLASH_SIZE` bytes starting at `--base` using PICOBOOT flash erase
commands. Direct erase requires both the base and size to be 4096-byte aligned.
Use `--backend uf2` to write `0xFF` data over the range through the BOOTSEL
mass-storage drive.

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

#### `reset` — reboot into BOOTSEL or normal mode

```sh
probe-rp-usb reset
probe-rp-usb reset --normal
```

Sends a vendor USB request to reboot the device into the ROM bootloader and
waits for the BOOTSEL mass-storage drive to appear. With `--normal`, sends a
PICOBOOT normal reboot request to a device already in BOOTSEL mode; if the
app-mode device is already present, the command succeeds without changing it.

---

#### `check` — check for a mounted BOOTSEL drive

```sh
probe-rp-usb check
```

Prints the mount path if a BOOTSEL drive is currently visible, otherwise exits
with an error.

## Library API

The crate exposes the same building blocks used by the CLI:

- `flash::flash` and `flash::flash_uf2` write ELF or raw binary inputs through
  the PICOBOOT or UF2 mass-storage backends.
- `write::*` reads, writes, and erases raw flash ranges.
- `attach::*` decodes `defmt` streams and can reconnect across resets.
- `usb::reset_to_bootsel` reboots app-mode firmware into BOOTSEL.
- `usb::reboot_to_normal` reboots a BOOTSEL/PICOBOOT device back into the
  normal application, or returns success if the app-mode device is already
  present.
- `elf::elf2uf2` and `uf2::*` provide the local ELF-to-UF2 and raw UF2 helpers.

Use the re-exported `probe_rp_usb::Family` enum for UF2 family IDs; downstream
crates do not need to depend on `elf2uf2-core`.

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
