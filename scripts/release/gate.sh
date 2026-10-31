#!/bin/sh
# Release gates. Each reads only its arguments (and the runs file) and writes one line.
#   gate.sh <tag> <sha> <on-main: true|false> <runs.json> <manifest-version>
#     exit 0 when <tag> may be released, else exit 1 naming the first failed check, in the order main, CI push run,
#     version. runs.json must be exactly one JSON array of CI workflow runs (conclusion, head_sha, event); anything
#     else, or any jq failure, refuses.
#   gate.sh --dist <version> <sha> <dist version> <dist sha>
#     whether the dist ref may move to <version> built from <sha>: "push" when dist is absent (empty dist version) or
#     older, "same" when dist already holds <version> from <sha> (a rerun), else exit 1 with "refused: ...".
# MUX_GATE_LIB=1 when sourcing defines the functions and runs nothing. Needs jq. POSIX sh.

set -eu

refuse() {
  echo "refused: $*"
  exit 1
}

# release_version <v>: succeeds when v is <major>.<minor>.<patch>, each a number without a leading zero.
release_version() {
  case $1 in '' | .* | *. | *..* | *[!0-9.]*) return 1 ;; esac
  case .$1. in *.0[0-9]*) return 1 ;; esac
  case $1 in *.*.*.*) return 1 ;; *.*.*) return 0 ;; *) return 1 ;; esac
}

# newer <a> <b>: succeeds when release version a is strictly greater than b, numerically part by part.
newer() {
  amajor=${1%%.*} apatch=${1##*.} bmajor=${2%%.*} bpatch=${2##*.}
  aminor=${1#*.} bminor=${2#*.}
  aminor=${aminor%.*} bminor=${bminor%.*}
  [ "$amajor" -ne "$bmajor" ] && { [ "$amajor" -gt "$bmajor" ]; return; }
  [ "$aminor" -ne "$bminor" ] && { [ "$aminor" -gt "$bminor" ]; return; }
  [ "$apatch" -gt "$bpatch" ]
}

# dist_decision <version> <sha> <dist version> <dist sha>: prints push or same, or refuses (exit 1).
dist_decision() {
  release_version "$1" || refuse "$1 is not a release version"
  if [ -z "$3" ]; then echo push; return 0; fi
  release_version "$3" || refuse "dist version $3 is not a release version"
  [ -n "$4" ] || refuse "dist holds $3 but no source commit"
  if newer "$1" "$3"; then echo push; return 0; fi
  if [ "$1" = "$3" ]; then
    [ "$2" = "$4" ] || refuse "dist already holds $3 from $4"
    echo same
    return 0
  fi
  refuse "dist holds $3, newer than $1"
}

usage() {
  echo 'usage: gate.sh <tag> <sha> <true|false> <runs.json> <manifest-version> | gate.sh --dist <version> <sha> <dist version> <dist sha>' >&2
  exit 2
}

# release_gate <tag> <sha> <on-main> <runs.json> <manifest-version>
release_gate() {
  tag=$1 sha=$2 on_main=$3 runs=$4 version=$5
  [ -n "$tag" ] && [ -n "$sha" ] || usage
  case $on_main in true | false) ;; *) echo "gate.sh: on-main must be true or false, not $on_main" >&2; exit 2 ;; esac

  [ "$on_main" = true ] || refuse "$tag is not on main"

  # Exactly one document, and it is an array: -s gathers every document, so [] [] or [..] junk fails here.
  if ! jq -e -s 'length == 1 and (.[0] | type) == "array"' "$runs" >/dev/null 2>&1; then
    refuse "cannot read CI runs from $runs"
  fi
  # "<push runs> <successful push runs>" for sha.
  if ! counts=$(jq -r --arg sha "$sha" '
      map(select(.event == "push" and .head_sha == $sha))
      | "\(length) \(map(select(.conclusion == "success")) | length)"' "$runs" 2>/dev/null); then
    refuse "cannot read CI runs from $runs"
  fi
  case $counts in
    *[!0-9\ ]* | '' | ' '* | *' ' | *' '*' '*) refuse "cannot read CI runs from $runs" ;;
  esac
  [ "${counts% *}" != 0 ] || refuse "no CI push run for $sha"
  [ "${counts#* }" != 0 ] || refuse "no successful CI push run for $sha"
  [ "v$version" = "$tag" ] || refuse "plugin.json version $version does not match $tag"

  echo "release ok: $tag at $sha"
}

main() {
  if [ "${1-}" = --dist ]; then
    [ $# -eq 5 ] || usage
    shift
    dist_decision "$@"
    return 0
  fi
  [ $# -eq 5 ] || usage
  release_gate "$@"
}

[ "${MUX_GATE_LIB-}" = 1 ] || main "$@"
