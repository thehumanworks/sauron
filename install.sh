#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-/usr/local}"
BIN_DIR="${BIN_DIR:-${PREFIX}/bin}"
BIN_DIR_EXPLICIT=0
BUILD_MATRIX="${BUILD_MATRIX:-1}"
INCLUDE_WINDOWS="${INCLUDE_WINDOWS:-0}"

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]

Build and install sauron for the current host, and (by default) build the
release matrix (best-effort):
  - macOS:   aarch64 + x86_64
  - Linux:   x86_64-unknown-linux-gnu

Options:
  --prefix <path>       Installation prefix (default: /usr/local)
  --bin-dir <path>      Installation bin directory (default: <prefix>/bin)
  --matrix              Build the release target matrix (default)
  --no-matrix           Only build/install the host target
  --windows             Also attempt Windows target build (best-effort)
  --no-windows          Disable Windows target build
  -h, --help            Show this help text

Env vars:
  PREFIX=/path          Same as --prefix
  BIN_DIR=/path         Same as --bin-dir
  BUILD_MATRIX=0|1      Same as --no-matrix / --matrix
  INCLUDE_WINDOWS=0|1   Same as --no-windows / --windows
EOF
}

log() {
  printf '[install] %s\n' "$*"
}

warn() {
  printf '[install] warning: %s\n' "$*" >&2
}

die() {
  printf '[install] error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || die "Missing required command: ${cmd}"
}

have_cargo_subcommand() {
  local subcommand="$1"
  cargo --list | grep -E "^[[:space:]]+${subcommand}[[:space:]]" >/dev/null 2>&1
}

install_cargo_subcommand() {
  local crate="$1"
  local subcommand="$2"
  if ! have_cargo_subcommand "${subcommand}"; then
    log "Installing cargo-${subcommand} (${crate})"
    cargo install --locked "${crate}"
  fi
}

build_target() {
  local target="$1"

  case "${target}" in
    aarch64-apple-darwin|x86_64-apple-darwin)
      if [[ "${HOST_TRIPLE}" == *-apple-darwin ]]; then
        cargo build --release --locked --target "${target}"
      else
        warn "Skipping ${target} (requires a macOS host with Apple SDK)"
      fi
      ;;
    x86_64-unknown-linux-gnu)
      if [[ "${HOST_TRIPLE}" == "x86_64-unknown-linux-gnu" ]]; then
        cargo build --release --locked --target "${target}"
      else
        if ! command -v zig >/dev/null 2>&1; then
          warn "Skipping ${target} (install zig to enable Linux cross-builds)"
          return 0
        fi
        install_cargo_subcommand cargo-zigbuild zigbuild
        cargo zigbuild --release --locked --target "${target}"
      fi
      ;;
    x86_64-pc-windows-msvc)
      if [[ "${HOST_TRIPLE}" == *-pc-windows-msvc ]]; then
        cargo build --release --locked --target "${target}"
      else
        warn "Skipping ${target} (cross-building Windows requires extra toolchain setup)"
      fi
      ;;
    *)
      die "Unsupported target in build matrix: ${target}"
      ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      [[ $# -ge 2 ]] || die "--prefix requires a value"
      PREFIX="$2"
      if [[ "${BIN_DIR_EXPLICIT}" != "1" ]]; then
        BIN_DIR="${PREFIX}/bin"
      fi
      shift 2
      ;;
    --bin-dir)
      [[ $# -ge 2 ]] || die "--bin-dir requires a value"
      BIN_DIR="$2"
      BIN_DIR_EXPLICIT=1
      shift 2
      ;;
    --matrix)
      BUILD_MATRIX=1
      shift
      ;;
    --no-matrix)
      BUILD_MATRIX=0
      shift
      ;;
    --windows)
      INCLUDE_WINDOWS=1
      shift
      ;;
    --no-windows)
      INCLUDE_WINDOWS=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown option: $1 (use --help)"
      ;;
  esac
done

need_cmd cargo
need_cmd rustup
need_cmd rustc
need_cmd install

HOST_TRIPLE="$(rustc -vV | awk -F': ' '/^host: / { print $2 }')"
[[ -n "${HOST_TRIPLE}" ]] || die "Could not determine host target triple from rustc"

log "Host target: ${HOST_TRIPLE}"
cd "${SCRIPT_DIR}"

rustup target add "${HOST_TRIPLE}" >/dev/null
log "Building host binary"
cargo build --release --locked --target "${HOST_TRIPLE}"

HOST_BIN="${SCRIPT_DIR}/target/${HOST_TRIPLE}/release/sauron"
if [[ "${HOST_TRIPLE}" == *-pc-windows-msvc ]]; then
  HOST_BIN="${HOST_BIN}.exe"
fi
[[ -f "${HOST_BIN}" ]] || die "Built binary not found at ${HOST_BIN}"

DEST_BIN="${BIN_DIR}/sauron"
if [[ "${HOST_TRIPLE}" == *-pc-windows-msvc ]]; then
  DEST_BIN="${DEST_BIN}.exe"
fi

if mkdir -p "${BIN_DIR}" 2>/dev/null; then
  if ! install -m 0755 "${HOST_BIN}" "${DEST_BIN}" 2>/dev/null; then
    need_cmd sudo
    sudo install -m 0755 "${HOST_BIN}" "${DEST_BIN}"
  fi
else
  need_cmd sudo
  sudo mkdir -p "${BIN_DIR}"
  sudo install -m 0755 "${HOST_BIN}" "${DEST_BIN}"
fi

log "Installed: ${DEST_BIN}"

if [[ "${BUILD_MATRIX}" == "1" ]]; then
  MATRIX_TARGETS=(
    aarch64-apple-darwin
    x86_64-apple-darwin
    x86_64-unknown-linux-gnu
  )
  if [[ "${INCLUDE_WINDOWS}" == "1" ]]; then
    MATRIX_TARGETS+=(x86_64-pc-windows-msvc)
  fi

  log "Building release matrix: ${MATRIX_TARGETS[*]}"
  rustup target add "${MATRIX_TARGETS[@]}" >/dev/null

  for target in "${MATRIX_TARGETS[@]}"; do
    log "Building target ${target}"
    build_target "${target}"
  done
fi

log "Done. Verify with: sauron --help"
