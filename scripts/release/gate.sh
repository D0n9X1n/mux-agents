#!/bin/sh
# Release gate: exit 0 when <tag> may be released, else exit 1 naming the first failed check, in the order main,
# CI push run, version. The workflow gathers the inputs with git and gh api; this script reads only its arguments
# and the runs file, and writes one line.
#   gate.sh <tag> <sha> <on-main: true|false> <runs.json> <manifest-version>
# runs.json is a JSON array of CI workflow runs, each with conclusion, head_sha and event; anything else refuses.
# Needs jq. POSIX sh.

if [ $# -ne 5 ] || [ -z "$1" ] || [ -z "$2" ]; then
  echo 'usage: gate.sh <tag> <sha> <true|false> <runs.json> <manifest-version>' >&2
  exit 2
fi
tag=$1 sha=$2 on_main=$3 runs=$4 version=$5
case $on_main in true | false) ;; *) echo "gate.sh: on-main must be true or false, not $on_main" >&2; exit 2 ;; esac

refuse() {
  echo "refused: $*"
  exit 1
}

[ "$on_main" = true ] || refuse "$tag is not on main"

# "<push runs> <successful push runs>" for sha, or nothing when runs.json is not an array of runs.
counts=$(jq -r --arg sha "$sha" '
  if type != "array" then error("not an array") else . end
  | map(select(.event == "push" and .head_sha == $sha))
  | "\(length) \(map(select(.conclusion == "success")) | length)"' "$runs" 2>/dev/null)
case $counts in [0-9]*' '[0-9]*) ;; *) refuse "cannot read CI runs from $runs" ;; esac
[ "${counts% *}" != 0 ] || refuse "no CI push run for $sha"
[ "${counts#* }" != 0 ] || refuse "no successful CI push run for $sha"
[ "v$version" = "$tag" ] || refuse "plugin.json version $version does not match $tag"

echo "release ok: $tag at $sha"
exit 0
