#!/bin/sh
# Tests for scripts/mux-agent, the POSIX entry shim. Run: sh tests/shim.sh
# Each case copies scripts/ into a plugin root with a space in its name and plants fake binaries that log their argv.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
root="$tmp/plug in"
shim="$root/scripts/mux-agent"

case $(uname -s) in Darwin) os=macos ;; Linux) os=linux ;; *) os=unknown ;; esac
case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) arch=unknown ;; esac
tier1="$tmp/env bin/mux-agent"
tier2="$root/scripts/bin/$os-$arch/mux-agent"
tier3="$root/target/release/mux-agent"

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1: $2"; }

# A fresh plugin root: scripts/ copied, no binaries, no MUX_AGENTS_BIN.
reset() {
  rm -rf "$root" "$tmp/env bin"
  mkdir -p "$root" "$tmp/env bin"
  cp -R "$repo/scripts" "$root/scripts"
  rm -rf "$root/scripts/bin"
  unset MUX_AGENTS_BIN
}

# fake <path> <label> [exit]: prints the label, then one argv entry per line wrapped in [ ], then exits.
fake() {
  mkdir -p "$(dirname "$1")"
  # shellcheck disable=SC2016 # "$@" and "$a" belong to the fake script
  printf '#!/bin/sh\necho %s\nfor a in "$@"; do printf "[%%s]\\n" "$a"; done\nexit %s\n' "$2" "${3:-0}" >"$1"
  chmod +x "$1"
}

# run [args...]: runs the shim; sets out and rc.
run() {
  out=$("$shim" "$@")
  rc=$?
}

expect() { # name want
  if [ "$rc" = 0 ] && [ "$out" = "$2" ]; then ok "$1"; else bad "$1" "rc=$rc out=$out"; fi
}

reset; fake "$tier1" tier1; export MUX_AGENTS_BIN="$tier1"; run list
expect tier1_env_wins "tier1
[list]"

reset; fake "$tier2" tier2; run list -All
expect tier2_bin_os_arch "tier2
[list]
[-All]"

reset; fake "$tier3" tier3; run list
expect tier3_target_release "tier3
[list]"

reset; fake "$tier1" tier1; fake "$tier2" tier2; fake "$tier3" tier3; export MUX_AGENTS_BIN="$tier1"
run; a=$out; unset MUX_AGENTS_BIN; run; b=$out; rm "$tier2"; run; c=$out
if [ "$a,$b,$c" = "tier1,tier2,tier3" ]; then ok precedence_1_2_3; else bad precedence_1_2_3 "$a,$b,$c"; fi

reset; fake "$tier2" tier2; chmod -x "$tier2"; fake "$tier3" tier3; run x
if [ "$rc" = 0 ] && [ "$out" = "tier2
[x]" ] && [ -x "$tier2" ]; then ok nonexec_gets_chmod; else bad nonexec_gets_chmod "rc=$rc out=$out"; fi

# A directory is never a binary, though -x holds for it: the shim moves on.
reset; mkdir -p "$tier1"; export MUX_AGENTS_BIN="$tier1"; fake "$tier3" tier3; run d
expect directory_skipped "tier3
[d]"

# A root-owned file that is not executable cannot be fixed by chmod, so the shim moves on. Root could fix it.
target=/etc/hosts
# shellcheck disable=SC3067 # test -O is in dash, bash, ksh, zsh and busybox sh, every sh this runs under
if [ "$(id -u)" = 0 ]; then
  echo "skip unfixable_skipped (running as uid 0)"
elif [ ! -f "$target" ] || [ -x "$target" ] || [ -O "$target" ]; then
  echo "skip unfixable_skipped ($target is not a root-owned non-executable file)"
else
  reset; mkdir -p "$(dirname "$tier2")"; ln -s "$target" "$tier2"; fake "$tier3" tier3; run y 2>/dev/null
  if [ "$rc" = 0 ] && [ "$out" = "tier3
[y]" ] && [ ! -x "$target" ]; then ok unfixable_skipped; else bad unfixable_skipped "rc=$rc out=$out"; fi
fi

reset; fake "$tier3" tier3
# shellcheck disable=SC2016 # '$HOME' is a literal argument the shim must pass through unexpanded
run send 'a b' '' "it's" '"q"' '$HOME' '*' 'x  y'
expect spaces_in_root_and_args "tier3
[send]
[a b]
[]
[it's]
[\"q\"]
[\$HOME]
[*]
[x  y]"

reset; fake "$tier3" tier3 7; run stop z
if [ "$rc" = 7 ] && [ "$out" = "tier3
[stop]
[z]" ]; then ok exit_code_forwarded; else bad exit_code_forwarded "rc=$rc out=$out"; fi

# A MUX_AGENTS_BIN with no slash names a file in the cwd, the one checked, never a namesake on PATH.
reset; fake "$tmp/cwd/qa-bin" in-cwd; fake "$tmp/path/qa-bin" on-path; export MUX_AGENTS_BIN=qa-bin
out=$(cd "$tmp/cwd" && PATH="$tmp/path:$PATH" "$shim" go); rc=$?
expect bare_bin_runs_cwd_file "in-cwd
[go]"

reset; export MUX_AGENTS_BIN="$tmp/missing"; run list
want="{\"ok\":false,\"verb\":null,\"code\":1,\"check\":\"entry\",\"message\":\"no usable mux-agent binary for $os-$arch\"}"
if [ "$rc" = 1 ] && [ "$out" = "$want" ]; then ok none_usable_prints_entry_json_exit_1; else bad none_usable_prints_entry_json_exit_1 "rc=$rc out=$out"; fi

# Every line of SKILL.md's Commands block runs through the shim with <entry> expanded to it and each placeholder to a
# fixture; the recording fake must log exactly the line's own words.
reset; fake "$tier1" tier1; export MUX_AGENTS_BIN="$tier1"
fx="$tmp/fx"; mkdir -p "$fx"; : >"$fx/task.md"; : >"$fx/brief.md"; : >"$fx/body.md"; : >"$fx/status.md"
block=$(awk '/^## Commands/ { c = 1; next } c && /^```/ { if (f) exit; f = 1; next } c && f' "$repo/skills/mux-agents/SKILL.md")
prefix="\"$shim\" "
n=0; why=
set -f
while IFS= read -r line; do
  n=$((n + 1))
  case $line in '"<entry>" '*) ;; *) why="$why; not run as \"<entry>\": $line"; continue ;; esac
  line=$(printf '%s\n' "$line" | sed -e "s#<entry>#$shim#" -e 's#<client>#claude#g' -e 's#<name>#claude-1#g' \
    -e 's#<seconds>#600#g' -e 's#<n>#20#g' -e "s#<task-file>#$fx/task.md#g" -e "s#<brief-file>#$fx/brief.md#g" \
    -e "s#<body-file>#$fx/body.md#g" -e "s#<status-file>#$fx/status.md#g")
  case $line in *'<'*) why="$why; unexpanded placeholder: $line"; continue ;; esac
  want=tier1
  for w in ${line#"$prefix"}; do want="$want
[$w]"; done
  got=$(eval "$line" </dev/null)
  [ "$got" = "$want" ] || why="$why; $line logged: $got"
done <<END_BLOCK
$block
END_BLOCK
set +f
unset MUX_AGENTS_BIN
if [ "$n" = 15 ] && [ -z "$why" ]; then ok skill_commands_run_through_shim; else bad skill_commands_run_through_shim "$n commands$why"; fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
