#!/bin/sh
# Upgrade test with the real Claude Code (and Copilot CLI when it is on PATH): the documented one-line upgrade moves
# an installed mux-agents to a new release. Run: sh tests/upgrade.sh (Linux or macOS; needs claude, cargo, git, jq).
#
# The user's own commands run unchanged: `claude plugin marketplace add D0n9X1n/mux-agents`, then
# `claude plugin install mux-agents@mux-agents`. git in an isolated HOME rewrites every github.com URL of that
# repository (url.insteadOf in $HOME/.gitconfig) to a local bare repository standing in for GitHub, so the marketplace
# is cloned and cached exactly as a GitHub marketplace is, and every git fetch stays on this machine. A release is
# published by pushing to that repository: main gets this commit's tree (its real marketplace.json pins ref dist) and
# dist is replaced by the plugin tree plus the packaged host binary. Claude Code's cache is never edited.
#
# HOME, CLAUDE_CONFIG_DIR, CLAUDE_CODE_PLUGIN_CACHE_DIR (the plugins root) and COPILOT_HOME point into a temp dir; git
# reads no system config. MUX_AGENTS_BIN is never set, so the shim must find the binary in the installed copy.
#   update_alone_keeps_cached_catalog: after the release, `claude plugin update` alone reads the cached catalog and
#     stays on 0.1.0. This pins the behaviour the one-liner exists for.
#   migrate_relative_to_dist: from 0.1.0 (entry "./" on main) the one-liner moves to 0.2.0 from dist.
#   bump_on_same_ref: dist replaced by 0.2.1 (not a fast-forward, as a release replaces dist); the one-liner moves on.
#   copilot_*: the same two upgrades with the Copilot CLI one-liner, or skipped when copilot is not on PATH.
# Each upgrade checks the installed version, the copy's plugin.json and binary (its SHA-256), and that the copy's shim
# runs that binary: outside a multiplexer it refuses with check endpoint.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd -P)
for tool in claude cargo git jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "upgrade.sh: $tool is required but not on PATH" >&2; exit 2; }
done
case $(uname -s) in Darwin) os=macos ;; Linux) os=linux ;; *) echo "upgrade.sh: Linux or macOS only" >&2; exit 2 ;; esac
case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) echo "upgrade.sh: unknown arch" >&2; exit 2 ;; esac
if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }; else sha() { shasum -a 256 "$1" | cut -d' ' -f1; }; fi
copilot=no
command -v copilot >/dev/null 2>&1 && copilot=yes

tmp=$(mktemp -d "${TMPDIR:-/tmp}/upgrade-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
tmp=$(cd "$tmp" && pwd -P)

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1: $2"; }
skip() { pass=$((pass + 1)); echo "skip $1: $2"; }

# The host binary, built with the real HOME (cargo and rustup live there).
if ! sh "$repo/scripts/release/package.sh" --host --out "$tmp/bin" >"$tmp/package.log" 2>&1; then
  cat "$tmp/package.log" >&2
  echo "upgrade.sh: packaging failed" >&2
  exit 1
fi
want_sha=$(sha "$tmp/bin/$os-$arch/mux-agent")

HOME="$tmp/home" CLAUDE_CONFIG_DIR="$tmp/claude" CLAUDE_CODE_PLUGIN_CACHE_DIR="$tmp/plugins" COPILOT_HOME="$tmp/copilot"
GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 CLAUDE_CODE_PLUGIN_PREFER_HTTPS=1
DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1
export HOME CLAUDE_CONFIG_DIR CLAUDE_CODE_PLUGIN_CACHE_DIR COPILOT_HOME GIT_CONFIG_NOSYSTEM GIT_TERMINAL_PROMPT
export CLAUDE_CODE_PLUGIN_PREFER_HTTPS DISABLE_AUTOUPDATER DISABLE_TELEMETRY DISABLE_ERROR_REPORTING
unset MUX_AGENTS_BIN MUX_AGENTS_STATE XDG_STATE_HOME XDG_CONFIG_HOME GIT_CONFIG_GLOBAL TMUX RMUX TMUX_PANE
mkdir -p "$HOME" "$CLAUDE_CONFIG_DIR" "$CLAUDE_CODE_PLUGIN_CACHE_DIR" "$COPILOT_HOME"
plugins=$CLAUDE_CODE_PLUGIN_CACHE_DIR
echo "claude $(command claude --version 2>&1)"
[ "$copilot" = yes ] && echo "copilot $(command copilot --version 2>&1 | head -n 1)"

# The documented one-line upgrades (README.md, Upgrade).
claude_upgrade() { command claude plugin marketplace update mux-agents && command claude plugin update mux-agents@mux-agents; }
copilot_upgrade() { command copilot plugin marketplace update mux-agents && command copilot plugin update mux-agents@mux-agents; }

# fgit <repo> <git args...>: git as the fixture author, unsigned.
fgit() { r=$1; shift; git -C "$r" -c user.name=fixture -c user.email=fixture@example.com -c commit.gpgsign=false "$@"; }
# set_version <dir> <version>: rewrites .claude-plugin/plugin.json's version.
set_version() {
  jq --arg v "$2" '.version = $v' "$1/.claude-plugin/plugin.json" >"$1/plugin.json.tmp" && mv "$1/plugin.json.tmp" "$1/.claude-plugin/plugin.json"
}

# --- GitHub, on this machine -------------------------------------------------------------------------------------
origin="$tmp/origin.git"
git init -q --bare -b main "$origin"
cat >"$HOME/.gitconfig" <<EOF
[url "file://$origin"]
	insteadOf = https://github.com/D0n9X1n/mux-agents.git
	insteadOf = https://github.com/D0n9X1n/mux-agents
	insteadOf = git@github.com:D0n9X1n/mux-agents.git
EOF

# main at 0.1.0, shaped as 0.1 shipped: the marketplace entry is the repository root.
work="$tmp/work"
mkdir -p "$work/.claude-plugin" "$work/scripts" "$work/skills/mux-agents"
printf '{"name":"mux-agents","version":"0.1.0","description":"mux-agents 0.1"}\n' >"$work/.claude-plugin/plugin.json"
printf '{"name":"mux-agents","owner":{"name":"fixture"},"plugins":[{"name":"mux-agents","source":"./","description":"mux-agents"}]}\n' \
  >"$work/.claude-plugin/marketplace.json"
cp "$repo/skills/mux-agents/SKILL.md" "$work/skills/mux-agents/SKILL.md"
echo 'the 0.1 entry' >"$work/scripts/mux-agent.ps1"
git init -q -b main "$work"
fgit "$work" add -A
fgit "$work" commit -q -m "v0.1.0"
fgit "$work" push -q "$origin" main

# publish <version>: dist replaced by this commit's plugin tree at <version> plus the binary; main gets this commit's
# tree, whose marketplace.json pins github ref dist.
publish() {
  d="$tmp/dist-$1"
  mkdir -p "$d"
  git -C "$repo" archive --format=tar HEAD | tar -x -C "$d"
  mkdir -p "$d/scripts/bin"
  cp -R "$tmp/bin/." "$d/scripts/bin/"
  set_version "$d" "$1"
  git init -q -b dist "$d"
  fgit "$d" add -A -f
  fgit "$d" commit -q -m "dist v$1"
  fgit "$d" push -q --force "$origin" dist
  find "$work" -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
  git -C "$repo" archive --format=tar HEAD | tar -x -C "$work"
  fgit "$work" add -A
  fgit "$work" commit -q --allow-empty -m "main for v$1"
  fgit "$work" push -q "$origin" main
}

claude_installed() { jq -r ".plugins[\"mux-agents@mux-agents\"][0].$1" "$plugins/installed_plugins.json" 2>/dev/null; }

# check_copy <case> <version> <install dir> <reported version> <update output>: the copy is <version>, holds the
# packaged binary, and its shim runs it.
check_copy() {
  manifest=$(jq -r .version "$3/.claude-plugin/plugin.json" 2>/dev/null)
  got_sha=$(sha "$3/scripts/bin/$os-$arch/mux-agent" 2>/dev/null)
  out=$(sh "$3/scripts/mux-agent" list 2>&1)
  rc=$?
  case $out in '{"ok":false,'*'"check":"endpoint"'*) shape=endpoint ;; *) shape=other ;; esac
  if [ "$4" = "$2" ] && [ "$manifest" = "$2" ] && [ "$got_sha" = "$want_sha" ] && [ ! -e "$3/target" ] \
    && [ "$rc" = 1 ] && [ "$shape" = endpoint ]; then
    ok "$1"
    echo "     $2 at $3; shim: $out"
  else
    bad "$1" "version=$4 manifest=$manifest (want $2) sha=$got_sha (want $want_sha) rc=$rc out=$out update: $5"
  fi
}

# check_claude <case> <version> <update output>
check_claude() {
  path=$(claude_installed installPath)
  if [ "$path" != "$plugins/cache/mux-agents/mux-agents/$2" ]; then
    bad "$1" "installPath=$path, want $plugins/cache/mux-agents/mux-agents/$2; update: $3"
    return
  fi
  check_copy "$1" "$2" "$path" "$(claude_installed version)" "$3"
}

# check_copilot <case> <version> <update output>
check_copilot() {
  path="$COPILOT_HOME/installed-plugins/mux-agents/mux-agents"
  check_copy "$1" "$2" "$path" "$(jq -r .version "$path/.claude-plugin/plugin.json" 2>/dev/null)" "$3"
}

# show <output>: the command's output on one indented line.
show() { printf '     %s\n' "$(printf '%s' "$1" | tr '\n' ' ')"; }

# --- install 0.1.0 the way a user does ---------------------------------------------------------------------------
setup=$( { command claude plugin marketplace add D0n9X1n/mux-agents && command claude plugin install mux-agents@mux-agents; } 2>&1)
if [ "$(claude_installed version)" = 0.1.0 ]; then echo "     claude: installed 0.1.0 at $(claude_installed installPath)"
else echo "$setup"; echo "upgrade.sh: the Claude Code 0.1.0 install did not take" >&2; exit 1; fi
if [ "$copilot" = yes ]; then
  setup=$( { command copilot plugin marketplace add D0n9X1n/mux-agents && command copilot plugin install mux-agents@mux-agents; } 2>&1)
  if [ "$(jq -r .version "$COPILOT_HOME/installed-plugins/mux-agents/mux-agents/.claude-plugin/plugin.json" 2>/dev/null)" = 0.1.0 ]; then
    echo "     copilot: installed 0.1.0"
  else echo "$setup"; echo "upgrade.sh: the Copilot CLI 0.1.0 install did not take" >&2; exit 1; fi
fi

# --- release 0.2.0 -------------------------------------------------------------------------------------------------
publish 0.2.0

update=$(command claude plugin update mux-agents@mux-agents 2>&1)
show "$update"
case $update in *"already at the latest version (0.1.0)"*) said=yes ;; *) said=no ;; esac
if [ "$(claude_installed version)" = 0.1.0 ] && [ "$said" = yes ]; then ok update_alone_keeps_cached_catalog
else bad update_alone_keeps_cached_catalog "version=$(claude_installed version); update: $update"; fi

update=$(claude_upgrade 2>&1)
show "$update"
check_claude migrate_relative_to_dist 0.2.0 "$update"

if [ "$copilot" = yes ]; then
  update=$(copilot_upgrade 2>&1)
  show "$update"
  check_copilot copilot_migrate_relative_to_dist 0.2.0 "$update"
else skip copilot_migrate_relative_to_dist "copilot is not on PATH"; fi

# --- release 0.2.1 on the same ref ---------------------------------------------------------------------------------
publish 0.2.1

update=$(claude_upgrade 2>&1)
show "$update"
check_claude bump_on_same_ref 0.2.1 "$update"

if [ "$copilot" = yes ]; then
  update=$(copilot_upgrade 2>&1)
  show "$update"
  check_copilot copilot_bump_on_same_ref 0.2.1 "$update"
else skip copilot_bump_on_same_ref "copilot is not on PATH"; fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
