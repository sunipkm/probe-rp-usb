#!/bin/sh
# shellcheck shell=dash
# shellcheck disable=SC2039  # local is non-POSIX but universally supported
#
# Installer for probe-rp-usb on Linux and macOS.
#
# Usage (one-liner):
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.sh \
#     | sh
#
# Options (pass via environment or CLI flags):
#   --version <tag>       Install a specific release tag (default: latest)
#   --no-modify-path      Skip PATH modification
#   --verbose, -v         Enable verbose output
#   --quiet,   -q         Suppress all output except errors
#   --help,    -h         Print this help text
#
# Environment overrides:
#   PROBE_RP_USB_VERSION=v0.2.0     Pin version
#   PROBE_RP_USB_INSTALL_DIR=/path  Override install directory
#   INSTALLER_NO_MODIFY_PATH=1      Skip PATH modification
#   INSTALLER_PRINT_VERBOSE=1       Verbose mode
#   INSTALLER_PRINT_QUIET=1         Quiet mode
#
# Licensed under MIT OR Apache-2.0.

# Some versions of ksh have no `local` keyword; alias it to typeset.
# mksh has this alias already.
has_local() {
    # shellcheck disable=SC2034
    local _has_local
}
has_local 2>/dev/null || alias local=typeset

set -u

# ── Constants ──────────────────────────────────────────────────────────────────

APP_NAME="probe-rp-usb"
REPO="sunipkm/probe-rp-usb"
RELEASES_BASE="https://github.com/$REPO/releases"
API_LATEST="https://api.github.com/repos/$REPO/releases/latest"
API_ALL="https://api.github.com/repos/$REPO/releases"

# ── Runtime config (may be overridden by env or CLI flags) ────────────────────

PRINT_VERBOSE=${INSTALLER_PRINT_VERBOSE:-0}
PRINT_QUIET=${INSTALLER_PRINT_QUIET:-0}
NO_MODIFY_PATH=${INSTALLER_NO_MODIFY_PATH:-0}
REQUESTED_VERSION=${PROBE_RP_USB_VERSION:-}
FORCE_INSTALL_DIR=${PROBE_RP_USB_INSTALL_DIR:-}

# ── Helpers ────────────────────────────────────────────────────────────────────

say() {
    if [ "0" = "$PRINT_QUIET" ]; then
        echo "$1"
    fi
}

say_verbose() {
    if [ "1" = "$PRINT_VERBOSE" ]; then
        echo "$1"
    fi
}

warn() {
    if [ "0" = "$PRINT_QUIET" ]; then
        local _red _reset
        _red=$(tput setaf 1 2>/dev/null || echo '')
        _reset=$(tput sgr0 2>/dev/null || echo '')
        say "${_red}WARN${_reset}: $1" >&2
    fi
}

err() {
    if [ "0" = "$PRINT_QUIET" ]; then
        local _red _reset
        _red=$(tput setaf 1 2>/dev/null || echo '')
        _reset=$(tput sgr0 2>/dev/null || echo '')
        say "${_red}ERROR${_reset}: $1" >&2
    fi
    exit 1
}

need_cmd() {
    if ! check_cmd "$1"; then
        err "required command not found: '$1'"
    fi
}

check_cmd() {
    command -v "$1" > /dev/null 2>&1
}

# Run a command that must not fail.
ensure() {
    if ! "$@"; then err "command failed: $*"; fi
}

# Run a command whose failure is intentionally ignored.
ignore() {
    "$@"
}

usage() {
    cat <<EOF
${APP_NAME}-installer.sh

Installs probe-rp-usb from GitHub Releases.

USAGE:
    ${APP_NAME}-installer.sh [OPTIONS]

OPTIONS:
    --version <tag>     Install a specific version, e.g. "v0.2.0" (default: latest)
    --no-modify-path    Do not add the install directory to PATH
    -v, --verbose       Enable verbose output
    -q, --quiet         Suppress all non-error output
    -h, --help          Print this help

ENVIRONMENT:
    PROBE_RP_USB_VERSION=<tag>       Pin version
    PROBE_RP_USB_INSTALL_DIR=<path>  Override install directory
    INSTALLER_NO_MODIFY_PATH=1       Equivalent to --no-modify-path
    INSTALLER_PRINT_VERBOSE=1        Equivalent to --verbose
    INSTALLER_PRINT_QUIET=1          Equivalent to --quiet
EOF
}

# ── Architecture detection ─────────────────────────────────────────────────────

get_bitness() {
    # Read the ELF class byte (offset 4) to determine 32 vs 64 bit.
    need_cmd head
    local _exe="$1"
    local _head
    _head=$(head -c 5 "$_exe")
    if [ "$_head" = "$(printf '\177ELF\001')" ]; then
        echo 32
    elif [ "$_head" = "$(printf '\177ELF\002')" ]; then
        echo 64
    else
        err "unknown ELF bitness for platform detection"
    fi
}

get_current_exe() {
    if test -L /proc/self/exe; then
        echo /proc/self/exe
    elif test -n "${SHELL:-}"; then
        echo "$SHELL"
    else
        need_cmd /bin/sh
        echo /bin/sh
    fi
}

get_architecture() {
    local _ostype _cputype _clibtype _bitness _current_exe
    _ostype=$(uname -s)
    _cputype=$(uname -m)
    _clibtype="gnu"

    if [ "$_ostype" = "Linux" ]; then
        if ldd --version 2>&1 | grep -q musl; then
            _clibtype="musl"
        fi
        _current_exe=$(get_current_exe)
        _bitness=$(get_bitness "$_current_exe")
    fi

    if [ "$_ostype" = "Darwin" ]; then
        # Detect Rosetta: uname -m can lie, use sysctl instead.
        if [ "$_cputype" = "x86_64" ]; then
            if sysctl hw.optional.arm64 2>/dev/null | grep -q ': 1'; then
                _cputype=arm64
            fi
        fi
        if [ "$_cputype" = "i386" ]; then
            if sysctl hw.optional.x86_64 2>/dev/null | grep -q ': 1'; then
                _cputype=x86_64
            fi
        fi
    fi

    case "$_ostype" in
        Linux)       _ostype="unknown-linux-${_clibtype}" ;;
        Darwin)      _ostype="apple-darwin" ;;
        FreeBSD)     _ostype="unknown-freebsd" ;;
        *)           err "unsupported OS: $_ostype" ;;
    esac

    case "$_cputype" in
        x86_64 | x86-64 | x64 | amd64)
            _cputype=x86_64
            ;;
        aarch64 | arm64)
            _cputype=aarch64
            ;;
        *)
            err "unsupported CPU architecture: $_cputype"
            ;;
    esac

    # Detect 64-bit Linux with a 32-bit userland (rare but possible).
    if [ "$_ostype" = "unknown-linux-${_clibtype}" ] && [ "${_bitness:-64}" -eq 32 ]; then
        err "32-bit userland on 64-bit Linux kernel is not supported"
    fi

    RETVAL="${_cputype}-${_ostype}"
}

# ── Downloader (curl or wget) ──────────────────────────────────────────────────

downloader() {
    # Detect broken snap-packaged curl (lacks permissions to access the network).
    local _use_curl=0
    if check_cmd curl; then
        if ! curl --version 2>/dev/null | grep -qi snap; then
            _use_curl=1
        else
            warn "snap-installed curl detected; it may lack network access — falling back to wget"
        fi
    fi

    if [ "$1" = "--check" ]; then
        if [ "$_use_curl" = "1" ] || check_cmd wget; then
            return 0
        fi
        err "neither curl nor wget found — cannot download files"
    fi

    # $1=url $2=destination
    local _url="$1"
    local _dst="$2"

    if [ "$_use_curl" = "1" ]; then
        curl --proto '=https' --tlsv1.2 -sSfL "$_url" -o "$_dst"
    elif check_cmd wget; then
        wget --https-only --secure-protocol=TLSv1_2 -qO "$_dst" "$_url"
    else
        err "neither curl nor wget found"
    fi
}

# ── Version resolution ─────────────────────────────────────────────────────────

resolve_version() {
    local _requested="$1"

    if [ -n "$_requested" ]; then
        # Normalise: accept "0.1.0" as well as "v0.1.0"
        case "$_requested" in
            v*) echo "$_requested" ;;
            *)  echo "v$_requested" ;;
        esac
        return
    fi

    say "Fetching latest release from GitHub…" >&2

    need_cmd grep
    local _tmpfile
    _tmpfile=$(ensure mktemp)

    # Try the latest stable release first.
    if downloader "$API_LATEST" "$_tmpfile" 2>/dev/null; then
        local _stable_tag
        _stable_tag=$(grep '"tag_name"' "$_tmpfile" | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
        if [ -n "$_stable_tag" ]; then
            ignore rm -f "$_tmpfile"
            echo "$_stable_tag"
            return
        fi
    fi

    # No stable release found; fall back to the most recent release
    # (which may be a pre-release).
    say "No stable release found; checking for pre-releases…" >&2

    if ! downloader "$API_ALL" "$_tmpfile"; then
        ignore rm -f "$_tmpfile"
        err "failed to fetch release metadata from $API_ALL"
    fi

    # The API returns releases newest-first; grab the first tag_name.
    local _tag
    _tag=$(grep '"tag_name"' "$_tmpfile" | head -n 1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
    ignore rm -f "$_tmpfile"

    if [ -z "$_tag" ]; then
        err "no releases (stable or pre-release) found for '$REPO'"
    fi
    say "Using pre-release: $_tag" >&2
    echo "$_tag"
}

# ── Checksum verification ──────────────────────────────────────────────────────

verify_checksum() {
    local _file="$1"
    local _checksum_file="$2"

    if ! check_cmd sha256sum && ! check_cmd shasum; then
        warn "neither sha256sum nor shasum found — skipping checksum verification"
        return 0
    fi

    local _expected
    _expected=$(awk '{print $1}' "$_checksum_file")

    local _actual
    if check_cmd sha256sum; then
        _actual=$(sha256sum -b "$_file" | awk '{print $1}')
    else
        _actual=$(shasum -a 256 -b "$_file" | awk '{print $1}')
    fi

    if [ "$_actual" != "$_expected" ]; then
        err "SHA-256 mismatch for '$_file':
  expected: $_expected
  actual:   $_actual"
    fi
    say_verbose "SHA-256 verified: $_actual"
}

# ── PATH helpers ───────────────────────────────────────────────────────────────

# Replace $HOME prefix in a path with the literal string '$HOME'
# so it stays late-bound in shell rc files.
replace_home() {
    local _path="$1"
    local _home="${HOME:-}"
    if [ -n "$_home" ]; then
        echo "$_path" | sed "s|^${_home}|\$HOME|"
    else
        echo "$_path"
    fi
}

# Write an env-sourcing script that prepends $_install_dir to $PATH if not
# already present.  Compatible with sh/bash/zsh.
write_env_script() {
    local _install_dir_expr="$1"
    local _env_script="$2"
    cat > "$_env_script" <<EOF
#!/bin/sh
# Prepend ${APP_NAME} install dir to PATH (added by ${APP_NAME}-installer.sh)
case ":\${PATH}:" in
    *:"${_install_dir_expr}":*) ;;
    *) export PATH="${_install_dir_expr}:\$PATH" ;;
esac
EOF
}

# Write a fish-compatible env script.
write_env_script_fish() {
    local _install_dir_expr="$1"
    local _env_script="$2"
    cat > "$_env_script" <<EOF
# Prepend ${APP_NAME} install dir to PATH (added by ${APP_NAME}-installer.sh)
if not contains "${_install_dir_expr}" \$PATH
    set -x PATH "${_install_dir_expr}" \$PATH
end
EOF
}

# Add a `. /path/to/env` line to an rc file if not already present.
# The env script itself must already exist before this function is called.
add_source_line_to_rc() {
    local _env_script_expr="$1"  # late-bound expression, e.g. $HOME/.cargo/env
    local _env_script_path="$2"  # actual filesystem path
    local _rcfile="$3"

    local _line=". \"${_env_script_expr}\""

    if grep -qF "$_line" "$_rcfile" 2>/dev/null; then
        say_verbose "$_rcfile already sources $_env_script_expr"
        return 0
    fi

    say_verbose "adding source line to $_rcfile"
    printf '\n%s\n' "$_line" >> "$_rcfile"
    return 1  # indicates a change was made
}

# Add the install dir to CI PATH (GitHub Actions).
add_to_ci_path() {
    local _install_dir="$1"
    if [ -n "${GITHUB_PATH:-}" ]; then
        ensure echo "$_install_dir" >> "$GITHUB_PATH"
    fi
}

modify_path() {
    local _install_dir="$1"
    local _install_dir_expr
    _install_dir_expr=$(replace_home "$_install_dir")

    local _env_script="${_install_dir}/env"
    local _env_script_expr="${_install_dir_expr}/env"
    local _fish_env_script="${_install_dir}/env.fish"
    local _fish_env_script_expr="${_install_dir_expr}/env.fish"

    # Always write the env scripts.
    write_env_script "$_install_dir_expr" "$_env_script"
    ensure mkdir -p "$(dirname "$_fish_env_script")"
    write_env_script_fish "$_install_dir_expr" "$_fish_env_script"

    local _changed=0

    # sh/bash/zsh rc files — try .profile first, then the others.
    local _home="${HOME:-}"
    if [ -n "$_home" ]; then
        for _rc in \
            "$_home/.profile" \
            "$_home/.bash_profile" \
            "$_home/.bash_login" \
            "$_home/.bashrc" \
            "$_home/.zshrc" \
            "$_home/.zshenv"
        do
            if [ -f "$_rc" ]; then
                add_source_line_to_rc "$_env_script_expr" "$_env_script" "$_rc" || _changed=1
            fi
        done

        # fish
        local _fish_conf_dir="${_home}/.config/fish/conf.d"
        if [ -d "$_fish_conf_dir" ] || check_cmd fish; then
            ensure mkdir -p "$_fish_conf_dir"
            local _fish_rc="${_fish_conf_dir}/${APP_NAME}.env.fish"
            local _fish_line="source \"${_fish_env_script_expr}\""
            if ! grep -qF "$_fish_line" "$_fish_rc" 2>/dev/null; then
                printf '\n%s\n' "$_fish_line" >> "$_fish_rc"
                _changed=1
            fi
        fi
    fi

    if [ "$_changed" = "1" ]; then
        say ""
        say "PATH updated.  To apply changes in the current shell, run:"
        say "    source \"${_env_script_expr}\""
    else
        say_verbose "PATH already contains $_install_dir — nothing to update"
    fi
}

# ── Main installer logic ───────────────────────────────────────────────────────

main() {
    # Parse CLI flags.
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --help | -h)
                usage
                exit 0
                ;;
            --quiet | -q)
                PRINT_QUIET=1
                shift
                ;;
            --verbose | -v)
                PRINT_VERBOSE=1
                shift
                ;;
            --no-modify-path)
                NO_MODIFY_PATH=1
                shift
                ;;
            --version)
                shift
                REQUESTED_VERSION="${1:-}"
                [ -n "$REQUESTED_VERSION" ] || err "--version requires an argument"
                shift
                ;;
            --version=*)
                REQUESTED_VERSION="${1#*=}"
                shift
                ;;
            *)
                err "unknown option: $1"
                ;;
        esac
    done

    # Prerequisite check.
    downloader --check
    need_cmd uname
    need_cmd mktemp
    need_cmd mkdir
    need_cmd rm
    need_cmd tar
    need_cmd chmod

    say ""
    say "${APP_NAME} installer"
    say "══════════════════════"
    say ""

    # 1. Resolve version.
    local _version
    _version=$(resolve_version "$REQUESTED_VERSION")
    say "Installing ${APP_NAME} ${_version}"

    # 2. Detect target triple.
    get_architecture || exit 1
    local _arch="$RETVAL"
    say_verbose "Detected target: $_arch"

    # 3. Resolve install directory.
    local _install_dir
    if [ -n "$FORCE_INSTALL_DIR" ]; then
        _install_dir="$FORCE_INSTALL_DIR"
    elif [ -n "${CARGO_HOME:-}" ]; then
        _install_dir="${CARGO_HOME}/bin"
    elif [ -n "${HOME:-}" ]; then
        _install_dir="${HOME}/.cargo/bin"
    else
        err "cannot determine install directory: \$HOME and \$CARGO_HOME are both unset"
    fi

    local _install_dir_expr
    _install_dir_expr=$(replace_home "$_install_dir")

    say "Install directory: $_install_dir_expr"

    # 4. Build download URLs.
    local _archive="${APP_NAME}-${_arch}.tar.xz"
    local _base_url="${RELEASES_BASE}/download/${_version}"
    local _archive_url="${_base_url}/${_archive}"
    local _checksum_url="${_archive_url}.sha256"

    say_verbose "Archive URL: $_archive_url"

    # 5. Create temp directory.
    local _tmpdir
    _tmpdir=$(ensure mktemp -d)
    local _archive_path="${_tmpdir}/${_archive}"
    local _checksum_path="${_tmpdir}/${_archive}.sha256"

    # 6. Download.
    say "Downloading ${_archive_url}…"
    if ! downloader "$_archive_url" "$_archive_path"; then
        ignore rm -rf "$_tmpdir"
        err "download failed: $_archive_url
Make sure release ${_version} has a build for target ${_arch}."
    fi

    # 7. Verify checksum (optional).
    if downloader "$_checksum_url" "$_checksum_path" 2>/dev/null; then
        verify_checksum "$_archive_path" "$_checksum_path"
    else
        warn "no checksum file published for this release — skipping verification"
    fi

    # 8. Extract.
    say_verbose "Extracting to ${_tmpdir}…"
    ensure tar xf "$_archive_path" --no-same-owner -C "$_tmpdir"

    # Find the binary (may be at root or inside a subdirectory).
    local _bin_src
    _bin_src=$(find "$_tmpdir" -name "${APP_NAME}" -type f 2>/dev/null | head -n 1)
    if [ -z "$_bin_src" ]; then
        ignore rm -rf "$_tmpdir"
        err "${APP_NAME} binary not found inside the archive"
    fi

    # 9. Install.
    ensure mkdir -p "$_install_dir"

    # Use an atomic install: write to a temp name then rename.
    local _bin_tmp="${_install_dir}/.${APP_NAME}.tmp.$$"
    ensure cp "$_bin_src" "$_bin_tmp"
    ensure chmod 0755 "$_bin_tmp"
    ensure mv "$_bin_tmp" "${_install_dir}/${APP_NAME}"

    ignore rm -rf "$_tmpdir"

    say "Installed ${APP_NAME} to ${_install_dir_expr}/${APP_NAME}"

    # 10. Update PATH (CI + shell rc files).
    add_to_ci_path "$_install_dir"

    # Check if the install dir is already on PATH.
    case ":${PATH}:" in
        *":${_install_dir}:"*)
            NO_MODIFY_PATH=1
            say_verbose "Install dir already on PATH — skipping rc-file modification"
            ;;
    esac

    if [ "$NO_MODIFY_PATH" = "0" ]; then
        modify_path "$_install_dir"
    else
        if [ "$INSTALLER_NO_MODIFY_PATH" = "1" ] || [ "$NO_MODIFY_PATH" = "1" ]; then
            say ""
            say "PATH modification skipped."
            say "Add this to your shell profile to use ${APP_NAME}:"
            say "    export PATH=\"${_install_dir_expr}:\$PATH\""
        fi
    fi

    say ""
    say "${APP_NAME} ${_version} installed successfully!"
    say ""
    say "Quick-start:"
    say "  ${APP_NAME} flash  firmware.elf    # flash via BOOTSEL"
    say "  ${APP_NAME} watch  firmware.elf    # stream defmt logs"
    say "  ${APP_NAME} run    firmware.elf    # flash + watch"
    say ""
    if [ "$(uname -s)" = "Linux" ]; then
        say "Linux udev note:"
        say "  For non-root USB access, install the provided udev rule once:"
        say "    sudo cp /path/to/99-probe-rp-usb.rules /etc/udev/rules.d/"
        say "    sudo udevadm control --reload-rules && sudo udevadm trigger"
        say "  Your user must also be in the 'plugdev' group."
        say ""
    fi
}

main "$@" || exit 1
