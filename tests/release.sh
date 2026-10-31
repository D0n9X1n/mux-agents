#!/bin/sh
# Tests for scripts/release/package.sh and scripts/release/gate.sh. Run: sh tests/release.sh
# The packaging cases build the host binary into a copied plugin root; the header and gate cases are pure (crafted
# files and fixture JSON, no network).
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
package="$repo/scripts/release/package.sh"
gate="$repo/scripts/release/gate.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

case $(uname -s) in Darwin) os=macos ;; Linux) os=linux ;; MINGW* | MSYS* | CYGWIN*) os=windows ;; *) os=unknown ;; esac
case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) arch=unknown ;; esac
exe=; [ "$os" = windows ] && exe=.exe
if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$@"; }; else sha() { shasum -a 256 "$@"; }; fi

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1: $2"; }

# --- package --host into a copied plugin root -------------------------------------------------------------------
root="$tmp/plug in"
mkdir -p "$root"
cp -R "$repo/scripts" "$root/scripts"
rm -rf "$root/scripts/bin"
bin="$root/scripts/bin/$os-$arch/mux-agent$exe"
log=$(sh "$package" --host --out "$root/scripts/bin" 2>&1)
prc=$?

# shellcheck disable=SC2012 # one known file; stat differs across platforms
mode=$(ls -l "$bin" 2>/dev/null | cut -c1-10)
if [ "$prc" = 0 ] && [ "$mode" = -rwxr-xr-x ]; then ok host_path_mode_0755; else bad host_path_mode_0755 "rc=$prc mode=$mode log=$log"; fi

sums="$root/scripts/bin/SHA256SUMS"
check=$(cd "$root/scripts/bin" 2>/dev/null && sha -c SHA256SUMS 2>&1)
src=$?
if [ "$prc" = 0 ] && [ "$src" = 0 ] && [ "$(wc -l <"$sums" 2>/dev/null | tr -d ' ')" = 1 ] \
  && grep -q " \*\{0,1\}$os-$arch/mux-agent$exe\$" "$sums"; then ok sha256sums_match; else bad sha256sums_match "rc=$src $check"; fi

# Outside a multiplexer the shim reaches the packaged binary, which refuses with check endpoint.
out=$(unset TMUX RMUX TMUX_PANE MUX_AGENTS_BIN; "$root/scripts/mux-agent" list 2>&1)
rc=$?
case $out in
  '{"ok":false,'*'"check":"endpoint"'*) shape=yes ;;
  *) shape=no ;;
esac
if [ "$rc" = 1 ] && [ "$shape" = yes ]; then ok shim_runs_packaged_binary; else bad shim_runs_packaged_binary "rc=$rc out=$out"; fi

# --- package --target for the other Apple arch ------------------------------------------------------------------
if [ "$os" != macos ]; then
  echo "skip cross_target_header: cross-linking Apple targets needs macOS"
  ok cross_target_header
else
  if [ "$arch" = aarch64 ]; then other=x86_64 want='cf fa ed fe 07 00 00 01'; else other=aarch64 want='cf fa ed fe 0c 00 00 01'; fi
  cross="$tmp/cross"
  log=$(sh "$package" --target "$other-apple-darwin" --os macos --arch "$other" --out "$cross" 2>&1)
  crc=$?
  got=$(od -An -tx1 -N8 "$cross/macos-$other/mux-agent" 2>/dev/null | tr -s ' \n' ' ' | sed 's/^ //; s/ $//')
  if [ "$crc" = 0 ] && [ "$got" = "$want" ]; then ok cross_target_header; else bad cross_target_header "rc=$crc header=$got log=$log"; fi
fi

# --- check_header over crafted files ----------------------------------------------------------------------------
h="$tmp/headers"
mkdir -p "$h"
# zeros <file> <count>: appends count zero bytes.
zeros() { head -c "$2" /dev/zero >>"$1"; }
# elf <file> <e_machine bytes as octal escapes>: 64-bit little-endian ELF, e_type 2, e_machine at offset 18.
elf() { printf '\177ELF\002\001\001' >"$1"; zeros "$1" 9; printf '\002\000' >>"$1"; printf %b "$2" >>"$1"; zeros "$1" 44; }
# pe <file> <machine bytes>: MZ, e_lfanew = 0x80 at 0x3c, then PE\0\0 and the machine.
pe() { printf 'MZ' >"$1"; zeros "$1" 58; printf '\200\000\000\000' >>"$1"; zeros "$1" 64; printf 'PE\000\000' >>"$1"; printf %b "$2" >>"$1"; zeros "$1" 18; }
# macho <file> <cputype bytes>: 64-bit little-endian Mach-O magic, then cputype.
macho() { printf '\317\372\355\376' >"$1"; printf %b "$2" >>"$1"; zeros "$1" 24; }

elf "$h/elf-x64" '\076\000'; elf "$h/elf-a64" '\267\000'
pe "$h/pe-x64" '\144\206'; pe "$h/pe-a64" '\144\252'
macho "$h/mo-x64" '\007\000\000\001'; macho "$h/mo-a64" '\014\000\000\001'
printf 'not a binary at all, just some text padding it out past sixty-four bytes........' >"$h/text"

# Each case: os arch file want(0 accepted / 1 refused).
cases='linux x86_64 elf-x64 0
linux aarch64 elf-a64 0
linux aarch64 elf-x64 1
linux x86_64 elf-a64 1
linux x86_64 pe-x64 1
windows x86_64 pe-x64 0
windows aarch64 pe-a64 0
windows aarch64 pe-x64 1
windows x86_64 pe-a64 1
windows x86_64 elf-x64 1
macos x86_64 mo-x64 0
macos aarch64 mo-a64 0
macos aarch64 mo-x64 1
macos x86_64 mo-a64 1
macos x86_64 elf-x64 1
linux x86_64 text 1
windows x86_64 text 1
macos aarch64 text 1
freebsd x86_64 elf-x64 1
linux riscv64 elf-x64 1'
why=
while read -r cos carch cfile cwant; do
  # shellcheck source=scripts/release/package.sh
  (MUX_PACKAGE_LIB=1 . "$package" && check_header "$cos" "$carch" "$h/$cfile") >/dev/null 2>&1
  got=$?
  [ "$got" = 0 ] || got=1
  [ "$got" = "$cwant" ] || why="$why; $cos $carch $cfile gave $got"
done <<END_CASES
$cases
END_CASES
if [ -z "$why" ]; then ok header_table_cases; else bad header_table_cases "$why"; fi

# --- gate.sh over fixture runs ----------------------------------------------------------------------------------
sha40=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
other40=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
g="$tmp/gate"
mkdir -p "$g"
run_json() { printf '{"conclusion":"%s","head_sha":"%s","event":"%s"}' "$1" "$2" "$3"; }
green=$(run_json success $sha40 push)
red=$(run_json failure $sha40 push)
printf '[%s]' "$green" >"$g/green.json"
printf '[]' >"$g/none.json"
printf '[%s]' "$red" >"$g/red.json"
printf '[%s,%s]' "$red" "$green" >"$g/red-green.json"
printf '[%s,%s]' "$green" "$red" >"$g/green-red.json"
printf '[%s,%s]' "$(run_json success $sha40 pull_request)" "$(run_json success $other40 push)" >"$g/not-push.json"
printf 'not json' >"$g/broken.json"
printf '{"workflow_runs":[%s]}' "$green" >"$g/object.json"

# gate_line <on-main> <runs file> <version>: "<exit> <stdout>".
gate_line() { line=$(sh "$gate" v0.1.0 $sha40 "$1" "$g/$2" "$3" 2>/dev/null); echo "$? $line"; }
okline="0 release ok: v0.1.0 at $sha40"
got=$(
  gate_line true green.json 0.1.0
  gate_line false green.json 0.1.0
  gate_line true none.json 0.1.0
  gate_line true red.json 0.1.0
  gate_line true green.json 0.2.0
  gate_line true not-push.json 0.1.0
  gate_line false none.json 0.2.0
  gate_line true none.json 0.2.0
  gate_line true red.json 0.2.0
  gate_line true red-green.json 0.1.0
  gate_line true green-red.json 0.1.0
  gate_line true broken.json 0.1.0
  gate_line true object.json 0.1.0
  gate_line maybe green.json 0.1.0
)
want="$okline
1 refused: v0.1.0 is not on main
1 refused: no CI push run for $sha40
1 refused: no successful CI push run for $sha40
1 refused: plugin.json version 0.2.0 does not match v0.1.0
1 refused: no CI push run for $sha40
1 refused: v0.1.0 is not on main
1 refused: no CI push run for $sha40
1 refused: no successful CI push run for $sha40
$okline
$okline
1 refused: cannot read CI runs from $g/broken.json
1 refused: cannot read CI runs from $g/object.json
2 "
if [ "$got" = "$want" ]; then ok gate_cases; else bad gate_cases "got:
$got"; fi

# --- assemble over a fake six-directory layout ------------------------------------------------------------------
a="$tmp/assemble"
for d in macos-aarch64 macos-x86_64 linux-x86_64 linux-aarch64; do mkdir -p "$a/$d"; echo "$d" >"$a/$d/mux-agent"; done
for d in windows-x86_64 windows-aarch64; do mkdir -p "$a/$d"; echo "$d" >"$a/$d/mux-agent.exe"; done
echo stale >"$a/SHA256SUMS"
echo stale >"$a/linux-x86_64/SHA256SUMS"
sh "$package" --assemble "$a" >/dev/null 2>&1
arc=$?
lines=$(wc -l <"$a/SHA256SUMS" | tr -d ' ')
verify=$(cd "$a" && sha -c SHA256SUMS 2>&1)
vrc=$?
left=$(cd "$a" && find . -mindepth 2 -name SHA256SUMS)
if [ "$arc" = 0 ] && [ "$lines" = 6 ] && [ "$vrc" = 0 ] && [ -z "$left" ]; then ok assembled_sums_cover_all_binaries
else bad assembled_sums_cover_all_binaries "rc=$arc lines=$lines verify=$vrc:$verify left=$left"; fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
