#!/bin/sh
# Benchmarks the release `mux-agent list` with six live agents: it must average at most 50 ms.
# Builds the release binary and the fake client, starts a private tmux server (its own socket in a temp dir, killed on
# exit), spawns six fake `claude` agents from a shell in its pane the way a user would, then runs hyperfine on `list`
# in that pane. HOME, MUX_AGENTS_STATE, XDG_STATE_HOME and TMPDIR all point into the temp dir; the user's own tmux,
# rmux and state are never touched. Prints the mean in ms; exits 1 when over budget or broken, 2 when a tool is
# missing. Needs cargo, hyperfine, jq and tmux.
set -eu

budget=0.050

for tool in cargo hyperfine jq tmux; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "bench-list.sh: $tool is required but not on PATH" >&2
        exit 2
    fi
done

here=$(cd "$(dirname "$0")" && pwd -P)
repo=$(cd "$here/../.." && pwd -P)
target=${CARGO_TARGET_DIR:-$repo/target}

tmp=
socket=
pane=
cleanup() {
    status=$?
    trap - EXIT INT TERM HUP
    if [ -n "$tmp" ] && [ -d "$tmp" ]; then
        # The fake clients leave when `quit` appears; then the private server goes.
        : >"$tmp/it/quit" 2>/dev/null || :
        if [ -S "$socket" ]; then
            server_pid=$(tmux -S "$socket" -N display-message -p '#{pid}' 2>/dev/null || :)
            tmux -S "$socket" -N kill-server >/dev/null 2>&1 || :
            n=0
            while [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null && [ "$n" -lt 50 ]; do
                pause
                n=$((n + 1))
            done
            if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
                kill -9 "$server_pid" 2>/dev/null || :
            fi
        fi
        # Give exiting clients a moment to stop writing before the dir goes.
        n=0
        while [ -n "$(client_pids)" ] && [ "$n" -lt 50 ]; do
            pause
            n=$((n + 1))
        done
        rm -rf "$tmp"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

pause() {
    sleep 0.1 2>/dev/null || sleep 1
}

# Fake clients of this run still alive.
client_pids() {
    [ -f "$tmp/it/clients.log" ] || return 0
    while read -r pid; do
        if kill -0 "$pid" 2>/dev/null; then
            echo "$pid"
        fi
    done <"$tmp/it/clients.log"
}

fail() {
    echo "bench-list.sh: $*" >&2
    if [ -S "$socket" ]; then
        echo "--- pane:" >&2
        tmux -S "$socket" -N capture-pane -p -t "$pane" >&2 2>/dev/null || :
    fi
    exit 1
}

# Short, so <tmp>/s fits a Unix socket path; canonical, since panes report real paths (/tmp is a link on macOS).
tmp=$(mktemp -d /tmp/mxbench.XXXXXX)
tmp=$(cd "$tmp" && pwd -P)
socket="$tmp/s"
mkdir "$tmp/bin" "$tmp/home" "$tmp/home/.claude" "$tmp/home/.claude/sessions" "$tmp/work" "$tmp/tmp" \
    "$tmp/xdg" "$tmp/it" "$tmp/out"
# mux-agent refuses a state root not mode 700.
mkdir -m 700 "$tmp/state"
exe="$tmp/bin/mux-agent"

# The benchmarked binary is the plain release build, copied before the `live` build of the fake client can touch it.
echo "building release mux-agent and fake-client" >&2
(cd "$repo" && cargo build --release --locked --bin mux-agent >&2)
cp "$target/release/mux-agent" "$exe"
(cd "$repo" && cargo build --release --locked --features live --bin fake-client >&2)
cp "$target/release/fake-client" "$tmp/bin/claude"
# The caller check refuses any non-shell process between the pane and the caller (`caller` 2), and hyperfine sits
# between the pane's shell and `list`. Basename `sh` is on the shell list, so the copy passes. Dev tooling only.
cp "$(command -v hyperfine)" "$tmp/sh"

# A cleared environment, so nothing of the caller's own session (TMUX, RMUX, state, CLAUDE_*) reaches the pane.
set -- HOME="$tmp/home" PATH="$tmp/bin:$PATH" TMPDIR="$tmp/tmp" \
    MUX_AGENTS_STATE="$tmp/state" XDG_STATE_HOME="$tmp/xdg" \
    MUX_IT_DIR="$tmp/it" MUX_IT_ENTRY="$exe"
for keep in USER LOGNAME LANG; do
    eval "value=\${$keep-}"
    if [ -n "$value" ]; then
        set -- "$@" "$keep=$value"
    fi
done
env -i "$@" tmux -S "$socket" -f /dev/null new-session -d -x 200 -y 50 -c "$tmp/work" sh
pane=$(tmux -S "$socket" -N list-panes -a -F '#{pane_id}')

# Runs one sh line in the pane, stdout and stderr to $tmp/out/<n>.{out,err}, the exit code to <n>.code, written
# last; waits up to $2 seconds. Sets $run to <n>.
runs=0
in_pane() {
    runs=$((runs + 1))
    run="$tmp/out/$runs"
    tmux -S "$socket" -N send-keys -t "$pane" -l \
        "$1 >$run.out 2>$run.err; echo \$? >$run.code.tmp; mv $run.code.tmp $run.code"
    tmux -S "$socket" -N send-keys -t "$pane" Enter
    n=0
    while [ ! -f "$run.code" ]; do
        [ "$n" -lt $(($2 * 10)) ] || fail "timed out: $1"
        pause
        n=$((n + 1))
    done
    code=$(cat "$run.code")
}

i=1
while [ "$i" -le 6 ]; do
    in_pane "$exe spawn -Client claude -Task bench-$i -Name bench-$i" 30
    [ "$code" = 0 ] || fail "spawn $i exited $code: $(cat "$run.out" "$run.err")"
    i=$((i + 1))
done

# Each launcher commits its agent once its pane is up; wait until list sees all six.
n=0
while :; do
    in_pane "$exe list" 30
    if [ "$code" = 0 ] && [ "$(jq -c '.running' "$run.out")" = 6 ]; then
        break
    fi
    [ "$n" -lt 60 ] || fail "list never reported 6 running: $(cat "$run.out" "$run.err")"
    pause
    n=$((n + 1))
done

# The probe: one plain run answers "running":6 with exit 0.
in_pane "$exe list" 30
[ "$code" = 0 ] || fail "probe list exited $code: $(cat "$run.err")"
grep -q '"running":6' "$run.out" || fail "probe list did not answer \"running\":6: $(cat "$run.out")"

in_pane "$tmp/sh -N --warmup 5 --export-json $tmp/out/bench.json '$exe list'" 300
[ "$code" = 0 ] || fail "hyperfine exited $code: $(cat "$run.out" "$run.err")"
# Wall-clock mean in seconds: hyperfine 2.x keeps it under summary.time_wall_clock, 1.x at the top of the result.
mean=$(jq -e '.results[0] | (.summary.time_wall_clock.mean // .mean) | numbers' "$tmp/out/bench.json") ||
    fail "no numeric mean in hyperfine's export: $(cat "$tmp/out/bench.json")"
ms=$(awk -v m="$mean" 'BEGIN { printf "%.1f", m * 1000 }')
if awk -v m="$mean" -v b="$budget" 'BEGIN { exit !(m <= b) }'; then
    echo "list with 6 agents: mean $ms ms (budget 50 ms)"
else
    echo "list with 6 agents: mean $ms ms, over the 50 ms budget" >&2
    exit 1
fi
