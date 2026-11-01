#!/bin/sh
# Upgrade test with the real Claude Code: one `claude plugin update` moves an installed mux-agents to a new version.
# Run: sh tests/upgrade.sh (Linux or macOS; needs claude, cargo, git and jq).
#
# Everything is local and isolated: HOME, CLAUDE_CONFIG_DIR and CLAUDE_CODE_PLUGIN_CACHE_DIR (the plugins root:
# installed_plugins.json, known_marketplaces.json and cache/) point into a temp dir, git reads no system or user
# config, and the plugin repositories are local git repositories. MUX_AGENTS_BIN is never set, so the shim must find
# the binary in the installed copy.
#
# The marketplace is a local directory (Claude Code refuses a file:// marketplace URL). Its entry first has
# "source": "./", as 0.1 did; the catalog then switches the entry to {"source": "url", "url": "file://<repo>",
# "ref": "dist-test"}, the closest local stand-in for the published {"source": "github", "ref": "dist"}: both are git
# sources with a ref, fetched and cached by the version in their plugin.json.
#   migrate_relative_to_dist: 0.1.0 from "./", then one update to 0.2.0 on dist-test.
#   bump_on_same_ref: dist-test is replaced by 0.2.1 (not a fast-forward, as a release replaces dist), then one update.
# Each checks installed_plugins.json, the cache path, the copy's plugin.json and binary, and that the copy's shim runs
# that binary: outside a multiplexer it refuses with check endpoint.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd -P)
for tool in claude cargo git jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "upgrade.sh: $tool is required but not on PATH" >&2; exit 2; }
done
case $(uname -s) in Darwin) os=macos ;; Linux) os=linux ;; *) echo "upgrade.sh: Linux or macOS only" >&2; exit 2 ;; esac
case $(uname -m) in arm64 | aarch64) arch=aarch64 ;; x86_64 | amd64) arch=x86_64 ;; *) echo "upgrade.sh: unknown arch" >&2; exit 2 ;; esac
if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }; else sha() { shasum -a 256 "$1" | cut -d' ' -f1; }; fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/upgrade-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
tmp=$(cd "$tmp" && pwd -P)

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1: $2"; }

# The host binary, built with the real HOME (cargo and rustup live there).
if ! sh "$repo/scripts/release/package.sh" --host --out "$tmp/bin" >"$tmp/package.log" 2>&1; then
  cat "$tmp/package.log" >&2
  echo "upgrade.sh: packaging failed" >&2
  exit 1
fi
want_sha=$(sha "$tmp/bin/$os-$arch/mux-agent")

HOME="$tmp/home" CLAUDE_CONFIG_DIR="$tmp/claude" CLAUDE_CODE_PLUGIN_CACHE_DIR="$tmp/plugins" GIT_CONFIG_NOSYSTEM=1
DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1
export HOME CLAUDE_CONFIG_DIR CLAUDE_CODE_PLUGIN_CACHE_DIR GIT_CONFIG_NOSYSTEM DISABLE_AUTOUPDATER DISABLE_TELEMETRY DISABLE_ERROR_REPORTING
unset MUX_AGENTS_BIN MUX_AGENTS_STATE XDG_STATE_HOME XDG_CONFIG_HOME GIT_CONFIG_GLOBAL TMUX RMUX TMUX_PANE
mkdir -p "$HOME" "$CLAUDE_CONFIG_DIR" "$CLAUDE_CODE_PLUGIN_CACHE_DIR"
plugins=$CLAUDE_CODE_PLUGIN_CACHE_DIR
echo "claude $(command claude --version 2>&1)"

# fgit <repo> <git args...>: git as the fixture author, unsigned.
fgit() { r=$1; shift; git -C "$r" -c user.name=fixture -c user.email=fixture@example.com -c commit.gpgsign=false "$@"; }
# set_version <dir> <version>: rewrites .claude-plugin/plugin.json's version.
set_version() {
  jq --arg v "$2" '.version = $v' "$1/.claude-plugin/plugin.json" >"$1/plugin.json.tmp" && mv "$1/plugin.json.tmp" "$1/.claude-plugin/plugin.json"
}

mkt=mux-upgrade
id="mux-agents@$mkt"
installed() { jq -r --arg id "$id" ".plugins[\$id][0].$1" "$plugins/installed_plugins.json" 2>/dev/null; }

# check <case> <version>: the install is <version> at its own cache path, holding the packaged binary, and its shim
# runs it.
check() {
  version=$(installed version) path=$(installed installPath)
  want_path="$plugins/cache/$mkt/mux-agents/$2"
  manifest=$(jq -r .version "$path/.claude-plugin/plugin.json" 2>/dev/null)
  got_sha=$(sha "$path/scripts/bin/$os-$arch/mux-agent" 2>/dev/null)
  out=$(sh "$path/scripts/mux-agent" list 2>&1)
  rc=$?
  case $out in '{"ok":false,'*'"check":"endpoint"'*) shape=endpoint ;; *) shape=other ;; esac
  if [ "$version" = "$2" ] && [ "$path" = "$want_path" ] && [ "$manifest" = "$2" ] && [ "$got_sha" = "$want_sha" ] \
    && [ ! -e "$path/target" ] && [ "$rc" = 1 ] && [ "$shape" = endpoint ]; then
    ok "$1"
    echo "     $version at $path; shim: $out"
  else
    bad "$1" "version=$version path=$path (want $want_path) manifest=$manifest sha=$got_sha (want $want_sha) rc=$rc out=$out update: $update"
  fi
}

# --- the 0.1.0-shaped install: a local marketplace whose entry is the plugin root ---------------------------------
market="$tmp/market"
mkdir -p "$market/.claude-plugin" "$market/scripts"
printf '{"name":"mux-agents","version":"0.1.0","description":"mux-agents 0.1"}\n' >"$market/.claude-plugin/plugin.json"
printf '{"name":"%s","owner":{"name":"fixture"},"plugins":[{"name":"mux-agents","source":"./","description":"mux-agents"}]}\n' "$mkt" \
  >"$market/.claude-plugin/marketplace.json"
echo 'the 0.1 entry' >"$market/scripts/mux-agent.ps1"
command claude plugin marketplace add "$market" >"$tmp/add.log" 2>&1 || { cat "$tmp/add.log"; exit 1; }
command claude plugin install "$id" >"$tmp/install.log" 2>&1 || { cat "$tmp/install.log"; exit 1; }
if [ "$(installed version)" = 0.1.0 ]; then echo "     installed 0.1.0 at $(installed installPath)"
else cat "$tmp/install.log"; echo "upgrade.sh: the 0.1.0 install did not take" >&2; exit 1; fi

# --- dist-test: the committed plugin tree at 0.2.0 plus the packaged binary --------------------------------------
dist="$tmp/dist"
mkdir -p "$dist"
git -C "$repo" archive --format=tar HEAD | tar -x -C "$dist"
mkdir -p "$dist/scripts/bin"
cp -R "$tmp/bin/." "$dist/scripts/bin/"
set_version "$dist" 0.2.0
git init -q -b dist-test "$dist"
fgit "$dist" add -A -f
fgit "$dist" commit -q -m "dist v0.2.0"
printf '{"name":"%s","owner":{"name":"fixture"},"plugins":[{"name":"mux-agents","source":{"source":"url","url":"file://%s","ref":"dist-test"},"description":"mux-agents"}]}\n' \
  "$mkt" "$dist" >"$market/.claude-plugin/marketplace.json"

# --- migrate_relative_to_dist ------------------------------------------------------------------------------------
update=$(command claude plugin update "mux-agents@$mkt" 2>&1)
echo "     $update" | tr '\n' ' '; echo
check migrate_relative_to_dist 0.2.0

# --- bump_on_same_ref: dist-test replaced by 0.2.1 ----------------------------------------------------------------
set_version "$dist" 0.2.1
fgit "$dist" add -A -f
fgit "$dist" commit -q --amend -m "dist v0.2.1"
update=$(command claude plugin update "mux-agents@$mkt" 2>&1)
echo "     $update" | tr '\n' ' '; echo
check bump_on_same_ref 0.2.1

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
