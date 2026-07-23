# probe-rp-usb

`probe-rp-usb` is a USB-only flash and debug utility for RP2040 and RP2350 devices.
It is designed for firmware workflows that rely on BOOTSEL/PICOBOOT for flashing
and `defmt` over CDC-ACM for logging.

When used with firmware that provides:

- a reset interface via [`rp-usb-reset`](https://crates.io/crates/rp-usb-reset), and
- a `defmt` USB stream via [`embassy-defmt-usb`](https://crates.io/crates/embassy-defmt-usb),

you can complete the full cycle over one cable:

`flash` &rarr; `reboot` &rarr; `attach/watch`

without SWD probes or button-driven BOOTSEL entry.

## Key capabilities

- Reset into BOOTSEL mode over USB
- Flash ELF and raw binaries through the PICOBOOT interface
- Use UF2 mass-storage as a compatibility backend
- Read exact flash ranges to files
- Decode `defmt` from CDC-ACM serial output
- Auto-reconnect in `watch` mode across resets and reflashes
- Use `run` for a combined flash-and-attach workflow

## Installation

### Installer script (prebuilt binaries)

Linux / macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.sh \
  | sh
```

Alternative with `wget`:

```sh
wget -qO- \
  https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.sh \
  | sh
```

Installer options:

| Option | Description |
| ------ | ----------- |
| `--version <tag>` | Install a specific release tag (for example, `v0.1.0`) |
| `--no-modify-path` | Skip PATH updates |
| `--verbose` / `--quiet` | Control installer verbosity |

Environment overrides: `PROBE_RP_USB_VERSION`, `PROBE_RP_USB_INSTALL_DIR`, `INSTALLER_NO_MODIFY_PATH`.

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.ps1 | iex"
```

Install a specific version or directory:

```powershell
.\probe-rp-usb-installer.ps1 -Version "v0.2.0" -InstallDir "$env:ProgramFiles\probe-rp-usb"
```

### Cargo

```sh
cargo install probe-rp-usb
```

### Local build/install

```sh
cargo install --path .
```

## Platform setup

### Linux: udev permissions

Non-root USB access requires a udev rule.

```sh
sudo cp 99-probe-rp-usb.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Ensure your user belongs to `plugdev` (USB) and `dialout` (serial):

```sh
sudo usermod -aG plugdev $USER   # USB device access
sudo usermod -aG dialout $USER   # serial port access
# log out and back in to apply changes
```

Covered vendor IDs:

| VID | Description |
| --- | ----------- |
| `2E8A` | Raspberry Pi app mode and BOOTSEL/PICOBOOT |
| `C0DE` | Custom fallback VID |
| `C001` | Custom fallback VID |

### macOS

No additional driver configuration is normally required. `nusb` uses `IOKit`
directly.

If serial access fails with permission errors, verify your user is not excluded
from `tty` group access.

### Windows

Serial operation (`attach`, `watch`, `run`) uses the built-in CDC-ACM class driver.

The `reset` and `flash` commands use vendor USB interfaces and require a
WinUSB-compatible binding on the relevant interface.

For firmware you control, use [`rp-usb-reset`](https://crates.io/crates/rp-usb-reset)
to expose the expected reset interface. On Windows 8.1 and newer, this is the
recommended integration and typically avoids manual Zadig driver setup.

If another driver is already bound to the required interface, replace it with
WinUSB using [Zadig](https://zadig.akeo.ie/):

1. Enable **Options -> List All Devices**.
2. Select the reset or PICOBOOT interface.
3. Choose **WinUSB**, then click **Install Driver** or **Replace Driver**.

The BOOTSEL mass-storage binding (`USBSTOR`) should be left unchanged.

## Usage

```text
probe-rp-usb [--vid <VID>] [--pid <PID>] <SUBCOMMAND>
```

`--vid` and `--pid` accept decimal or `0x`-prefixed hexadecimal values.
If omitted, defaults are VID `0x2E8A` and PID `0x0009`, with fallback scanning
for VID `0xC0DE` and `0xC001`.

ELF inputs are converted with the local `probe_rp_usb::elf` module. Raw binaries
are converted to UF2 pages or written directly through PICOBOOT, depending on
the command/backend.

### Subcommands

#### `run` - flash and attach

One-command equivalent to flash + watch.

```sh
probe-rp-usb run target/thumbv8m.main-none-eabihf/release/my-firmware
```

Options:

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--family` | `rp2xxx-absolute` | UF2 family (`rp2040`, `rp2xxx-absolute`, `rp2xxx-data`, `rp2350-arm-s`, `rp2350-arm-ns`, `rp2350-riscv`) |
| `--address` | `0x10000000` | Flash base address for raw binary inputs |
| `--port` | auto-detect | Override serial port |

#### `flash` - flash firmware

```sh
probe-rp-usb flash target/thumbv8m.main-none-eabihf/release/my-firmware
```

Writes firmware via the PICOBOOT interface by default, rebooting to BOOTSEL if
required. Use `--backend uf2` to flash through BOOTSEL mass-storage.

Options:

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--backend` | `picoboot` | `picoboot` (direct USB) or `uf2` (mass-storage) |
| `--family` | `rp2xxx-absolute` | UF2 family tag |
| `--address` | `0x10000000` | Flash base address for raw binary inputs |
| `--no-wait` | disabled | Leave device in BOOTSEL mode after flashing |

#### `write` - write raw binary ranges

```sh
probe-rp-usb write settings.bin@0x10040000 assets.bin@0x10100000
```

Writes one or more files to explicit flash addresses. The PICOBOOT backend uses
sector read/modify/erase/write so unaligned writes preserve neighboring bytes.

Use `--base` to offset all `FILE@OFFSET` entries and `--backend uf2` for
mass-storage fallback.

#### `read` - read flash range

```sh
probe-rp-usb read 0x10000000 0x10000 firmware.bin
```

Reads `LENGTH` bytes from `ADDRESS` into `OUTPUT` via PICOBOOT.

#### `erase` - erase flash range

```sh
probe-rp-usb erase 0x400000
```

Erases `FLASH_SIZE` bytes from `--base` via PICOBOOT erase commands. Direct
erase requires 4096-byte alignment for both base and size. Use `--backend uf2`
to perform an overwrite erase with `0xFF` data through BOOTSEL mass-storage.

#### `watch` - stream defmt with reconnect

```sh
probe-rp-usb watch target/thumbv8m.main-none-eabihf/release/my-firmware
```

Decodes `defmt` output from CDC-ACM and reconnects automatically after reset
or reflash events.

#### `attach` - stream defmt once

```sh
probe-rp-usb attach target/thumbv8m.main-none-eabihf/release/my-firmware
```

Like `watch`, but exits when the serial port closes.

#### `reset` - reboot device mode

```sh
probe-rp-usb reset
probe-rp-usb reset --normal
```

`reset` requests BOOTSEL entry via the firmware reset interface. `reset --normal`
requests application reboot for a device currently in BOOTSEL/PICOBOOT mode.

#### `check` - detect BOOTSEL mount

```sh
probe-rp-usb check
```

Prints the BOOTSEL mount path when present, otherwise returns an error.

## Library API

The crate exposes the same primitives used by the CLI:

- `flash::flash`, `flash::flash_uf2` for firmware flashing
- `write::*` for raw flash read/write/erase operations
- `attach::*` for `defmt` stream decoding and reconnect handling
- `usb::reset_to_bootsel` for BOOTSEL entry from app mode
- `usb::reboot_to_normal` for BOOTSEL-to-app reboot
- `elf::elf2uf2` and `uf2::*` for conversion and UF2 helpers

Use the re-exported `probe_rp_usb::Family` enum for UF2 family IDs.
