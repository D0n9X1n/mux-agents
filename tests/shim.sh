#!/bin/sh
# Tests for scripts/mux-agent, the POSIX entry shim. Run: sh tests/shim.sh
# Each case copies scripts/ into a plugin root with a space in its name and plants fake binaries that log their argv.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
root="$tmp/plug in"
shim="$root/scripts/mux-agent"

exe=mux-agent
case $(uname -s) in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  MINGW* | MSYS* | CYGWIN*) os=windows exe=mux-agent.exe ;;
  *) os=unknown ;;
esac
case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) arch=unknown ;; esac
tier1="$tmp/env bin/mux-agent"
tier2="$root/scripts/bin/$os-$arch/$exe"
tier3="$root/target/release/$exe"

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

# NTFS has no execute bit: Git Bash reports a #! file executable whatever chmod did, so the case means nothing there.
if [ "$os" = windows ]; then
  echo "skip nonexec_gets_chmod (no execute bit on Windows)"
else
  reset; fake "$tier2" tier2; chmod -x "$tier2"; fake "$tier3" tier3; run x
  if [ "$rc" = 0 ] && [ "$out" = "tier2
[x]" ] && [ -x "$tier2" ]; then ok nonexec_gets_chmod; else bad nonexec_gets_chmod "rc=$rc out=$out"; fi
fi

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
if [ "$n" = 18 ] && [ -z "$why" ]; then ok skill_commands_run_through_shim; else bad skill_commands_run_through_shim "$n commands$why"; fi

# #84: Copilot reads the plugin root's hooks.json (Claude Code reads hooks/hooks.json only) and runs each command from
# the plugin root. Every hook reports through the shim, never fails the turn, and maps Copilot's events as herdr's
# agents report: a prompt works, a question blocks until answered, a stop idles.
copilot_hooks_report_through_the_shim() {
  name=copilot_hooks_report_through_the_shim
  hooks="$repo/hooks.json"
  got=$(python3 - "$hooks" <<'PY'
import json, sys
h = json.load(open(sys.argv[1]))
assert h["version"] == 1, h
for event, entries in sorted(h["hooks"].items()):
    for e in entries:
        assert e["type"] == "command" and e["timeoutSec"] <= 10, e
        # Unix runs bash, Windows the portable command (no PowerShell in the repository, tests/repo.rs); both quiet.
        assert set(e) == {"type", "bash", "command", "timeoutSec"} | ({"matcher"} if "matcher" in e else set()), e
        assert e["bash"].startswith("sh scripts/mux-agent report ") and e["bash"].endswith(" -Quiet"), e
        words = e["bash"][len("sh scripts/mux-agent "):]
        assert e["command"] == "scripts\\mux-agent.cmd " + words, e
        print(f"{event}:{e.get('matcher', '')}:{words}")
PY
  ) || { bad "$name" "hooks.json shape"; return; }
  got=$(printf '%s\n' "$got" | tr -d '\r')
  want="agentStop::report idle -Quiet
postToolUse:ask_user:report working -Quiet
postToolUseFailure:ask_user:report working -Quiet
preToolUse:ask_user:report blocked -Kind question -Quiet
userPromptSubmitted::report working -Quiet"
  [ "$got" = "$want" ] || { bad "$name" "events: $got"; return; }
  # Run as Copilot runs it, from the plugin root, with a binary that records its arguments and fails loudly: the shim
  # passes them on, and the hook needs no shell to quiet it (-Quiet is mux-agent's own).
  fake="$tmp/fake-mux-agent"
  printf '#!/bin/sh\necho "$*" >> "%s/argv"\nexit 0\n' "$tmp" > "$fake"
  chmod +x "$fake"
  : > "$tmp/argv"
  python3 -c 'import json,sys; [print(e["bash"]) for es in json.load(open(sys.argv[1]))["hooks"].values() for e in es]' "$hooks" |
    tr -d '\r' |
    while IFS= read -r cmd; do
      (cd "$repo" && MUX_AGENTS_BIN="$fake" sh -c "$cmd") || echo "FAILED $cmd" >> "$tmp/argv"
    done
  ran=$(sort "$tmp/argv")
  want="report blocked -Kind question -Quiet
report idle -Quiet
report working -Quiet
report working -Quiet
report working -Quiet"
  if [ "$ran" = "$want" ]; then ok "$name"; else bad "$name" "ran: $ran"; fi
}
copilot_hooks_report_through_the_shim

# Shell shortcuts (scripts/shell/mux-agents.sh): sourced from a bash or zsh profile, it defines cc and gg, which run
# the plugin's entry as `up -Client claude|copilot -- <args>`. Each case runs a script in a fresh shell with no rc file
# against a copy of the plugin layout whose scripts/mux-agent is a fake that prints a label and each argument as [arg].
# SHIM_SHELLS names the shells (default: bash zsh); one that is missing is skipped locally and fails in CI.
# The integration case (cc and gg through the real scripts/mux-agent to a built binary's `up`) is tests/shortcuts.rs.
sc_root="$tmp/sc plug"
sc_file="$sc_root/scripts/shell/mux-agents.sh"
sc_home="$tmp/sc home"
sc_else="$tmp/sc else"
sc_glob="$tmp/sc glob"
sc_snap="$tmp/sc snap"
cases="$tmp/sc cases"
mkdir -p "$cases"
unset MUX_AGENTS_HOME

# sc_fake <path> <label>: an entry that prints the label and each argument as [arg], then exits FAKE_EXIT (0 by default).
sc_fake() {
  mkdir -p "$(dirname "$1")"
  # shellcheck disable=SC2016 # the expansions belong to the fake
  printf '#!/bin/sh\necho %s\nfor a in "$@"; do printf "[%%s]\\n" "$a"; done\nexit "${FAKE_EXIT:-0}"\n' "$2" >"$1"
  chmod +x "$1"
}

# sc_reset: a fresh plugin layout whose entry is the fake `entry`, a home layout whose entry is the fake `home`, a copy
# and a symlink of the shortcut file elsewhere, and a folder holding a file that `*` matches.
sc_reset() {
  rm -rf "$sc_root" "$sc_home" "$sc_else" "$sc_glob" "$sc_snap"
  mkdir -p "$sc_root/scripts" "$sc_else" "$sc_glob" "$sc_snap"
  cp -R "$repo/scripts/shell" "$sc_root/scripts/shell"
  sc_fake "$sc_root/scripts/mux-agent" entry
  sc_fake "$sc_home/scripts/mux-agent" home
  cp "$sc_file" "$sc_else/copy.sh"
  ln -s "$sc_file" "$sc_else/link.sh" 2>/dev/null
  : >"$sc_glob/match-me"
  unset FAKE_EXIT MUX_AGENTS_HOME
}

# sc_run <shell> <dir> <case> [args...]: runs the case script under the shell, with no rc file, from dir; sets out,
# err and rc.
sc_run() {
  sc_sh=$1 sc_dir=$2 sc_case=$3
  shift 3
  case $sc_sh in
    zsh) set -- zsh -f "$cases/$sc_case" "$@" ;;
    *) set -- "$sc_sh" --norc --noprofile "$cases/$sc_case" "$@" ;;
  esac
  out=$(cd "$sc_dir" && BASH_ENV='' ENV='' "$@" 2>"$tmp/sc err")
  rc=$?
  err=$(cat "$tmp/sc err")
}

# sc_up <label> <client> [args...]: what the fake prints for `up -Client <client> -- <args>`.
sc_up() {
  printf '%s\n[up]\n[-Client]\n[%s]\n[--]' "$1" "$2"
  shift 2
  for a in "$@"; do printf '\n[%s]' "$a"; done
}

sc_expect() { # name want [rc]
  if [ "$rc" = "${3:-0}" ] && [ "$out" = "$2" ] && [ -z "$err" ]; then ok "$1"; else bad "$1" "rc=$rc out=$out err=$err"; fi
}

# sc_grew <kind> [names...]: the snapshot after sourcing is the one before plus the names.
sc_grew() {
  k=$1
  shift
  [ "$({ cat "$sc_snap/before.$k"; for a in "$@"; do printf '%s\n' "$a"; done; } | LC_ALL=C sort)" = "$(LC_ALL=C sort "$sc_snap/after.$k")" ]
}

cat >"$cases/args" <<'EOF'
. "$1" || exit 90
cc a 'b c' '' '*' '$x' 'q"t' "it's" -r /tmp/f --dir=/tmp/f
EOF
cat >"$cases/gg" <<'EOF'
. "$1" || exit 90
gg x
EOF
cat >"$cases/exit" <<'EOF'
. "$1" || exit 90
export FAKE_EXIT=7
cc z
EOF
cat >"$cases/path" <<'EOF'
PATH=$2:$PATH
. "$1" || exit 90
cc p
EOF
cat >"$cases/away" <<'EOF'
. "$1" || exit 90
cd "$2" || exit 91
cc h
EOF
cat >"$cases/keep" <<'EOF'
__mux_agents_init() { echo mine; }
. "$1" || exit 90
__mux_agents_init
cc k
EOF
cat >"$cases/wargs" <<'EOF'
. "$1" || exit 90
cc /tmp/f --dir=/tmp/f
EOF
# snap <file> <folder> missing|twice: the options, function names and variable names before and after sourcing the
# file (twice for `twice`, which then runs cc). The zsh lines sit in eval so bash never parses them.
cat >"$cases/snap" <<'EOF'
SNAP=$2
snap() {
  if [ -n "${ZSH_VERSION-}" ]; then
    set -o >"$SNAP/$1.opts"
    eval 'print -rl -- ${(k)functions}' | LC_ALL=C sort >"$SNAP/$1.funcs"
    eval 'print -rl -- ${(k)parameters}' | LC_ALL=C sort >"$SNAP/$1.vars"
  else
    { set -o; shopt; } >"$SNAP/$1.opts"
    compgen -A function | LC_ALL=C sort >"$SNAP/$1.funcs"
    compgen -v | LC_ALL=C sort >"$SNAP/$1.vars"
  fi
}
snap warm
snap before
. "$1"
if [ "$3" = twice ]; then . "$1"; fi
snap after
if [ "$3" = twice ]; then cc t; fi
EOF

shortcut_cases() {
  s=$1
  sc_reset; sc_run "$s" "$sc_glob" args "$sc_file"
  # shellcheck disable=SC2016 # '$x' is a literal argument
  sc_expect "${s}_cc_args_arrive_exactly" "$(sc_up entry claude a 'b c' '' '*' '$x' 'q"t' "it's" -r /tmp/f --dir=/tmp/f)"

  sc_reset; sc_run "$s" "$tmp" gg "$sc_file"
  sc_expect "${s}_gg_runs_copilot" "$(sc_up entry copilot x)"

  sc_reset; sc_run "$s" "$tmp" exit "$sc_file"
  sc_expect "${s}_exit_code_passes_through" "$(sc_up entry claude z)" 7

  sc_reset; sc_fake "$tmp/sc path/mux-agent" on-path; sc_run "$s" "$tmp" path "$sc_file" "$tmp/sc path"
  sc_expect "${s}_mux_agent_on_path_never_used" "$(sc_up entry claude p)"

  # An empty MUX_AGENTS_HOME counts as unset: the sourced file's plugin runs.
  sc_reset; MUX_AGENTS_HOME=; export MUX_AGENTS_HOME; sc_run "$s" "$tmp" gg "$sc_file"; unset MUX_AGENTS_HOME
  sc_expect "${s}_empty_home_uses_the_sourced_plugin" "$(sc_up entry copilot x)"

  # Sourcing touches cc, gg and __mux_agents_entry only: a caller's own function of another name survives.
  sc_reset; sc_run "$s" "$tmp" keep "$sc_file"
  sc_expect "${s}_caller_function_survives_sourcing" "mine
$(sc_up entry claude k)"

  # MUX_AGENTS_HOME, absolute and relative to where the file is sourced, names the plugin for a copied or symlinked
  # file, and still does after a cd. Git Bash's ln -s may copy instead of linking; then the link cases are skipped,
  # except in CI on Linux and macOS, where they must run.
  for f in copy link; do
    if [ "$f" = link ]; then
      sc_reset
      if [ ! -L "$sc_else/link.sh" ]; then
        if [ -n "${CI-}" ] && { [ "$os" = linux ] || [ "$os" = macos ]; }; then
          bad "${s}_home_link_cases" "ln -s made no symlink"
        else
          echo "skip ${s}_home_*_link_after_cd (ln -s made no symlink)"
        fi
        continue
      fi
    fi
    for h in abs rel; do
      sc_reset
      if [ "$h" = abs ]; then MUX_AGENTS_HOME=$sc_home; else MUX_AGENTS_HOME=${sc_home#"$tmp"/}; fi
      export MUX_AGENTS_HOME
      sc_run "$s" "$tmp" away "$sc_else/$f.sh" "$sc_glob"
      unset MUX_AGENTS_HOME
      sc_expect "${s}_home_${h}_${f}_after_cd" "$(sc_up home claude h)"
    done
  done

  sc_reset; sc_run "$s" "$sc_root/scripts" away shell/mux-agents.sh "$sc_glob"
  sc_expect "${s}_relative_source_after_cd" "$(sc_up entry claude h)"

  sc_reset; rm "$sc_root/scripts/mux-agent"; sc_run "$s" "$tmp" snap "$sc_file" "$sc_snap" missing
  warn="mux-agents: no entry at $sc_root/scripts/shell/../../scripts/mux-agent; source scripts/shell/mux-agents.sh from the plugin folder or set MUX_AGENTS_HOME"
  if [ "$rc" = 0 ] && [ -z "$out" ] && [ "$err" = "$warn" ] && sc_grew opts && sc_grew funcs && sc_grew vars; then
    ok "${s}_missing_entry_warns_and_defines_nothing"
  else
    bad "${s}_missing_entry_warns_and_defines_nothing" "rc=$rc out=$out err=$err"
  fi

  sc_reset; sc_run "$s" "$tmp" snap "$sc_file" "$sc_snap" twice
  if [ "$rc" = 0 ] && [ "$out" = "$(sc_up entry claude t)" ] && [ -z "$err" ] && sc_grew opts && sc_grew funcs cc gg &&
    sc_grew vars __mux_agents_entry; then
    ok "${s}_sourcing_twice_adds_three_names_only"
  else
    bad "${s}_sourcing_twice_adds_three_names_only" "rc=$rc out=$out err=$err diff: $(cd "$sc_snap" && for k in opts funcs vars; do LC_ALL=C sort "before.$k" >"b.$k"; LC_ALL=C sort "after.$k" >"a.$k"; diff "b.$k" "a.$k"; done)"
  fi
}

for s in ${SHIM_SHELLS:-bash zsh}; do
  if ! command -v "$s" >/dev/null 2>&1; then
    if [ -n "${CI-}" ]; then bad "${s}_shortcuts" "$s is not installed"; else echo "skip ${s}_shortcuts ($s is not installed)"; fi
    continue
  fi
  shortcut_cases "$s"
done

# Git Bash's real boundary: a native Windows program, the recorder tests/shim.ps1 builds, receives cc's arguments as
# MSYS rewrites them for any native program (a POSIX-looking path becomes a Windows one), and no path-conversion
# variable is set in its environment: neither cc nor the shim sets one.
windows_boundary() {
  unset MSYS2_ARG_CONV_EXCL MSYS_NO_PATHCONV
  rec="$tmp/rec"
  mkdir -p "$rec"
  awk -v q="'" '$0 ~ ("-Value @" q "$") { c = 1; next } index($0, q "@") == 1 { c = 0 } c' "$repo/tests/shim.ps1" >"$rec/stub.cs"
  csc="$(cygpath -u "${WINDIR:-C:/Windows}")/Microsoft.NET/Framework64/v4.0.30319/csc.exe"
  "$csc" -nologo "-out:$(cygpath -w "$rec/mux-agent.exe")" "$(cygpath -w "$rec/stub.cs")" >/dev/null
  if [ ! -f "$rec/mux-agent.exe" ]; then bad windows_recorder "csc built nothing"; return; fi
  echo rec >"$rec/label.txt"
  w_root="$tmp/w plug"
  w_file="$w_root/scripts/shell/mux-agents.sh"
  mkdir -p "$w_root"
  cp -R "$repo/scripts" "$w_root/scripts"
  rm -rf "$w_root/scripts/bin"
  f=$(cygpath -m /tmp/f)
  envf="$tmp/rec env"
  export SHIM_ENV_FILE
  SHIM_ENV_FILE=$(cygpath -w "$envf")

  # Through MUX_AGENTS_BIN and the real shim, called via cc.
  export MUX_AGENTS_BIN="$rec/mux-agent.exe"
  rm -f "$envf"; sc_run bash "$tmp" wargs "$w_file"
  out=$(printf '%s\n' "$out" | tr -d '\r')
  sc_expect windows_cc_paths_get_the_msys_rewrite "$(sc_up rec claude "$f" "--dir=$f")"
  got=$(tr -d '\r' <"$envf" 2>/dev/null)
  if [ "$got" = 'unset|unset' ]; then ok windows_no_conversion_variable_reaches_the_program; else bad windows_no_conversion_variable_reaches_the_program "env=$got"; fi
  unset MUX_AGENTS_BIN

  # No MUX_AGENTS_BIN: the shim selects scripts/bin/windows-<arch>/mux-agent.exe.
  mkdir -p "$w_root/scripts/bin/windows-$arch"
  cp "$rec/mux-agent.exe" "$w_root/scripts/bin/windows-$arch/mux-agent.exe"
  echo bin >"$w_root/scripts/bin/windows-$arch/label.txt"
  rm -f "$envf"; sc_run bash "$tmp" wargs "$w_file"
  out=$(printf '%s\n' "$out" | tr -d '\r')
  sc_expect "windows_shim_selects_bin_windows_${arch}" "$(sc_up bin claude "$f" "--dir=$f")"
  got=$(tr -d '\r' <"$envf" 2>/dev/null)
  if [ "$got" = 'unset|unset' ]; then ok windows_bin_sees_no_conversion_variable; else bad windows_bin_sees_no_conversion_variable "env=$got"; fi
  unset SHIM_ENV_FILE
}
if [ "$os" = windows ]; then windows_boundary; fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
