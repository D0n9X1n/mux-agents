#!/bin/sh
# Packages mux-agent into <out>/<os>-<arch>/mux-agent[.exe] and writes <out>/SHA256SUMS.
#   package.sh --host [--out <dir>]                                  build for this machine
#   package.sh --target <triple> --os <os> --arch <arch> [--out <dir>]  cross-build one target
#   package.sh --assemble <dir>                                      only rewrite <dir>/SHA256SUMS
# <out> defaults to scripts/bin. A build runs cargo build --release --locked, refuses a binary whose header names
# another os or arch, ad-hoc signs it on macOS, makes it 0755 and reassembles the sums. assemble is the only
# SHA256SUMS writer: one file over every <dir>/*/mux-agent*, replacing any earlier one, with no per-directory sums.
# MUX_PACKAGE_LIB=1 when sourcing defines the functions and runs nothing. POSIX sh.

die() {
  echo "package.sh: $*" >&2
  exit 1
}

usage() {
  echo 'usage: package.sh --host | --target <triple> --os <macos|linux|windows> --arch <x86_64|aarch64> [--out <dir>] | --assemble <dir>' >&2
  exit 2
}

# hex <file> <offset> <count>: the bytes as lowercase hex pairs separated by single spaces.
hex() {
  od -An -tx1 -j "$2" -N "$3" "$1" 2>/dev/null | tr -s ' \n' ' ' | sed 's/^ //; s/ $//'
}

# check_header <os> <arch> <file>: succeeds when file is an executable image for os and arch.
# ELF: 64-bit little-endian, e_machine at 18 (3e 00 x86_64, b7 00 aarch64). PE: machine after PE\0\0 at the
# offset held in 0x3c (64 86 x86_64, 64 aa aarch64). Mach-O: 64-bit little-endian magic, cputype at 4
# (07 00 00 01 x86_64, 0c 00 00 01 aarch64).
check_header() {
  case $1-$2 in
    linux-x86_64) magic='7f 45 4c 46 02 01' at=18 want='3e 00' ;;
    linux-aarch64) magic='7f 45 4c 46 02 01' at=18 want='b7 00' ;;
    windows-x86_64) magic='4d 5a' want='64 86' ;;
    windows-aarch64) magic='4d 5a' want='64 aa' ;;
    macos-x86_64) magic='cf fa ed fe' at=4 want='07 00 00 01' ;;
    macos-aarch64) magic='cf fa ed fe' at=4 want='0c 00 00 01' ;;
    *) echo "package.sh: no header rule for $1-$2" >&2; return 1 ;;
  esac
  [ -f "$3" ] || { echo "package.sh: $3 is not a file" >&2; return 1; }
  n=$(( ${#magic} / 3 + 1 ))
  [ "$(hex "$3" 0 "$n")" = "$magic" ] || { echo "package.sh: $3 is not a $1 image" >&2; return 1; }
  if [ "$1" = windows ]; then
    lfanew=$(hex "$3" 60 4)
    [ "${#lfanew}" = 11 ] || { echo "package.sh: $3 has no PE offset" >&2; return 1; }
    # e_lfanew is little-endian: reverse the four bytes into one hex number.
    pe=$(( 0x$(printf '%s\n' "$lfanew" | awk '{ print $4 $3 $2 $1 }') ))
    [ "$(hex "$3" "$pe" 4)" = '50 45 00 00' ] || { echo "package.sh: $3 has no PE signature" >&2; return 1; }
    at=$(( pe + 4 ))
  fi
  got=$(hex "$3" "$at" "$(( ${#want} / 3 + 1 ))")
  [ "$got" = "$want" ] || { echo "package.sh: $3 is not $1 $2 (machine $got, want $want)" >&2; return 1; }
}

# sum256 <files...>: sha256 lines, from sha256sum or shasum -a 256, whichever exists.
sum256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# assemble <dir>: writes <dir>/SHA256SUMS over every <dir>/*/mux-agent* and removes any per-directory SHA256SUMS.
assemble() {
  [ -d "$1" ] || die "$1 is not a directory"
  (
    cd "$1" || exit 1
    set -- */mux-agent*
    [ -f "$1" ] || die "no */mux-agent* under $(pwd)"
    rm -f SHA256SUMS ./*/SHA256SUMS
    sum256 "$@" >SHA256SUMS.tmp || die "cannot hash $*"
    mv SHA256SUMS.tmp SHA256SUMS
  ) || exit 1
}

# host_os_arch: sets os and arch for this machine, named as scripts/mux-agent names them.
host_os_arch() {
  case $(uname -s) in Darwin) os=macos ;; Linux) os=linux ;; MINGW* | MSYS* | CYGWIN*) os=windows ;; *) die "unsupported host $(uname -s)" ;; esac
  case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) die "unsupported host arch $(uname -m)" ;; esac
}

main() {
  mode='' triple='' os='' arch='' out=''
  while [ $# -gt 0 ]; do
    case $1 in
      --host) mode=host; shift ;;
      --target) [ $# -ge 2 ] || usage; mode=target triple=$2; shift 2 ;;
      --os) [ $# -ge 2 ] || usage; os=$2; shift 2 ;;
      --arch) [ $# -ge 2 ] || usage; arch=$2; shift 2 ;;
      --out) [ $# -ge 2 ] || usage; out=$2; shift 2 ;;
      --assemble) [ $# -eq 2 ] || usage; assemble "$2"; return ;;
      *) usage ;;
    esac
  done
  case $mode in
    host) [ -z "$os$arch" ] || usage; host_os_arch ;;
    target) [ -n "$triple" ] && [ -n "$os" ] && [ -n "$arch" ] || usage ;;
    *) usage ;;
  esac

  case $0 in */*) here=${0%/*} ;; *) here=. ;; esac
  repo=$(cd "$here/../.." && pwd) || die "cannot find the repository"
  # A relative --out is relative to the caller's directory.
  case $out in '') out=$repo/scripts/bin ;; /* | [A-Za-z]:*) ;; *) out=$(pwd)/$out ;; esac

  exe=
  [ "$os" = windows ] && exe=.exe
  cd "$repo" || die "cannot enter $repo"
  targets=${CARGO_TARGET_DIR:-target}
  if [ "$mode" = host ]; then
    cargo build --release --locked || die 'cargo build failed'
    built=$targets/release/mux-agent$exe
  else
    cargo build --release --locked --target "$triple" || die "cargo build --target $triple failed"
    built=$targets/$triple/release/mux-agent$exe
  fi

  dest=$out/$os-$arch/mux-agent$exe
  mkdir -p "$out/$os-$arch" || die "cannot create $out/$os-$arch"
  cp "$built" "$dest" || die "cannot copy $built"
  check_header "$os" "$arch" "$dest" || { rm -f "$dest"; exit 1; }
  if [ "$os" = macos ]; then codesign -f -s - "$dest" || die "codesign failed for $dest"; fi
  chmod 0755 "$dest" || die "cannot chmod $dest"
  assemble "$out"
  echo "packaged $dest"
}

[ "${MUX_PACKAGE_LIB-}" = 1 ] || main "$@"
