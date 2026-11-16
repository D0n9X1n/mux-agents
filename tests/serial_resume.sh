#!/bin/sh
# occupy-3 (calls 19, 24): the live tests that run `resume` (tests/serial_resume.txt) run alone, every other live test
# in parallel. libtest names a test by its full path (Unix: `e2e::<mux>::<name>`; Windows: `<name>`), so each listed
# name is resolved to every registered test whose last path segment it is, and both passes select by those full names
# with --exact.
#   sh tests/serial_resume.sh parallel <test target> [libtest args]  every registered test but the listed ones
#   sh tests/serial_resume.sh serial <test target> [libtest args]    the listed ones, one at a time
#   sh tests/serial_resume.sh check <test target>                    the self-test: the resolver on a synthetic list,
#                                                                    then, from libtest's own --list of each pass,
#                                                                    every listed test is in the serial pass and not
#                                                                    in the parallel pass, and no test is lost
# Test names are Rust paths (no blanks, no glob characters), so they pass unquoted with globbing off.
set -euf

mode=${1:?usage: serial_resume.sh <parallel|serial|check> <test target> [libtest args]}
target=${2:?usage: serial_resume.sh <parallel|serial|check> <test target> [libtest args]}
shift 2
list=$(dirname "$0")/serial_resume.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo_test() {
    cargo test --features live,test-panic --locked --test "$target" -- "$@"
}

# The full names the target selects with libtest args "$2..", one per line, into file $1.
selected() {
    out=$1
    shift
    cargo_test --list "$@" >"$tmp/raw"
    sed -n 's/: test$//p' "$tmp/raw" | sort >"$out"
}

# The registered names (file $2) that list file $1 names; comments and blank lines skipped.
resolve() {
    awk 'NR == FNR { sub(/\r$/, ""); if (NF && $1 !~ /^#/) want[$1] = 1; next }
        { n = split($0, part, "::"); if (part[n] in want) print }' "$1" "$2"
}

# The libtest selection of pass $1, from the resolved names in $tmp/serial.
pass_args() {
    case $1 in
    parallel)
        printf '%s\n' --exact
        while read -r n; do printf '%s\n' --skip "$n"; done <"$tmp/serial"
        ;;
    serial)
        printf '%s\n' --exact --test-threads=1
        cat "$tmp/serial"
        ;;
    esac
}

# $tmp/serial: this target's resolved names; none is an error (a serial pass with no filter runs every test).
resolved() {
    selected "$tmp/all"
    resolve "$list" "$tmp/all" | sort >"$tmp/serial"
    if [ ! -s "$tmp/serial" ]; then
        echo "serial_resume: no test listed in $list is registered in $target" >&2
        exit 1
    fi
}

fail() {
    echo "serial_resume check $target: $*" >&2
    exit 1
}

case $mode in
parallel)
    resolved
    # shellcheck disable=SC2046 # one test name per line, unquoted on purpose
    cargo_test $(pass_args parallel) "$@"
    ;;
serial)
    resolved
    # shellcheck disable=SC2046 # one test name per line, unquoted on purpose
    cargo_test $(pass_args serial) "$@"
    ;;
check)
    # The resolver: a bare listed name finds every module-qualified registration and the bare one, never a longer name
    # that ends in it, nor a name only in a comment.
    printf '%s\n' '# a comment naming e2e_only' '' stop_then_resume >"$tmp/list"
    printf '%s\n' e2e::tmux::stop_then_resume e2e::rmux::stop_then_resume stop_then_resume \
        e2e::tmux::hc_stop_then_resume e2e::tmux::stop_then_resume_twice e2e::tmux::e2e_only >"$tmp/registered"
    printf '%s\n' e2e::rmux::stop_then_resume e2e::tmux::stop_then_resume stop_then_resume >"$tmp/want"
    resolve "$tmp/list" "$tmp/registered" | sort >"$tmp/got"
    diff "$tmp/want" "$tmp/got" >&2 || fail "the resolver on the synthetic list (want < > got)"
    # The real passes, as libtest lists them with the arguments each pass runs with.
    resolved
    # shellcheck disable=SC2046 # one test name per line, unquoted on purpose
    selected "$tmp/in_serial" $(pass_args serial)
    # shellcheck disable=SC2046 # one test name per line, unquoted on purpose
    selected "$tmp/in_parallel" $(pass_args parallel)
    diff "$tmp/serial" "$tmp/in_serial" >&2 || fail "the serial pass runs other than the listed tests (listed < > ran)"
    both=$(comm -12 "$tmp/serial" "$tmp/in_parallel")
    [ -z "$both" ] || fail "listed tests also run in the parallel pass: $both"
    sort -m "$tmp/in_serial" "$tmp/in_parallel" | diff "$tmp/all" - >&2 || fail "a test runs in neither pass"
    echo "serial_resume check $target: $(wc -l <"$tmp/serial") serial, $(wc -l <"$tmp/in_parallel") parallel, of $(wc -l <"$tmp/all")"
    while read -r n; do echo "  serial: $n"; done <"$tmp/serial"
    ;;
*)
    echo "serial_resume: unknown mode $mode" >&2
    exit 2
    ;;
esac
