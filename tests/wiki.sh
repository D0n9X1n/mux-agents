#!/bin/sh
# Tests for scripts/wiki/publish.sh, the publisher that mirrors wiki/ into the GitHub wiki repository.
# Run: sh tests/wiki.sh. Each case builds its own source folder and wiki repository under a temp dir; git reads no
# user or system config, so a signing or hook setting on the host cannot change a fixture.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
publisher="$repo/scripts/wiki/publish.sh"
# An explicit template: macOS mktemp -d ignores TMPDIR, and a case-sensitive TMPDIR is how macOS runs the case test.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/wiki-test.XXXXXX")
trap 'chmod -R u+rw "$tmp" 2>/dev/null; rm -rf "$tmp"' EXIT INT TERM
sha=0123456789abcdef0123456789abcdef01234567

HOME="$tmp/home"
mkdir -p "$HOME"
GIT_CONFIG_NOSYSTEM=1
export HOME GIT_CONFIG_NOSYSTEM
unset XDG_CONFIG_HOME GIT_CONFIG_GLOBAL GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL GITHUB_OUTPUT

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   $1"; }
bad() { fail=$((fail + 1)); echo "FAIL $1: $2"; }
skip() { pass=$((pass + 1)); echo "skip $1: $2"; }

# fgit <repo> <git args...>: git in repo as the fixture author, unsigned.
fgit() { r=$1; shift; git -C "$r" -c user.name=fixture -c user.email=fixture@example.com -c commit.gpgsign=false "$@"; }

# put <dir> <name> <text>: writes text (no newline added) to dir/name, making folders.
put() { mkdir -p "$(dirname "$1/$2")" && printf '%s' "$3" >"$1/$2"; }

# source_dir <name> [<file> <text>]...: a plain folder holding the files; prints its path.
source_dir() {
  d="$tmp/$1"; shift
  mkdir -p "$d"
  while [ "$#" -ge 2 ]; do put "$d" "$1" "$2"; shift 2; done
  echo "$d"
}

# wiki_repo <name> <branch> [<file> <text>]...: a repository on branch with one commit of the files; prints its path.
wiki_repo() {
  d="$tmp/$1" b=$2; shift 2
  mkdir -p "$d"
  git init -q -b "$b" "$d"
  while [ "$#" -ge 2 ]; do put "$d" "$1" "$2"; shift 2; done
  fgit "$d" add --all && fgit "$d" commit -q -m seed
  echo "$d"
}

# publish [args...]: runs the publisher with GITHUB_OUTPUT unset; sets out (stdout and stderr) and rc.
publish() { out=$(sh "$publisher" "$@" 2>&1); rc=$?; }
# publish_to <output file> [args...]: the same with GITHUB_OUTPUT set to the file.
publish_to() { o=$1; shift; out=$(GITHUB_OUTPUT=$o sh "$publisher" "$@" 2>&1); rc=$?; }

head_of() { fgit "$1" rev-parse HEAD; }
dirty() { fgit "$1" status --porcelain | wc -l | tr -d ' '; }

# --- usage ------------------------------------------------------------------------------------------------------
src=$(source_dir usage-src Home.md home)
wiki=$(wiki_repo usage-wiki master Home.md home)
usage='usage: publish.sh <source-dir> <wiki-repo> <source-sha>'
got=
publish "" "$wiki" "$sha"; got="$got$rc $out;"
publish "$src" "" "$sha"; got="$got$rc $out;"
publish "$src" "$wiki" ""; got="$got$rc $out;"
publish "$src" "$wiki"; got="$got$rc $out;"
publish; got="$got$rc $out;"
want="2 $usage;2 $usage;2 $usage;2 $usage;2 $usage;"
if [ "$got" = "$want" ]; then ok usage_exits_2; else bad usage_exits_2 "$got"; fi
publish "$src" "$wiki" "$sha" extra
if [ "$rc" = 2 ] && [ "$out" = "$usage" ]; then ok usage_refuses_extra_argument; else bad usage_refuses_extra_argument "rc=$rc $out"; fi

# --- refusals ---------------------------------------------------------------------------------------------------
wiki=$(wiki_repo nosrc-wiki master Home.md home)
publish "$tmp/no-such-dir" "$wiki" "$sha"
if [ "$rc $out" = "1 wiki source is not a directory: $tmp/no-such-dir" ]; then ok source_not_a_directory
else bad source_not_a_directory "rc=$rc $out"; fi

src=$(source_dir nogit-src Home.md home)
plain=$(source_dir nogit-wiki Home.md home)
publish "$src" "$plain" "$sha"
if [ "$rc $out" = "1 wiki destination is not a git worktree: $plain" ]; then ok destination_not_a_worktree
else bad destination_not_a_worktree "rc=$rc $out"; fi

src=$(source_dir branch-src Home.md new)
wiki=$(wiki_repo branch-wiki main Home.md old)
publish "$src" "$wiki" "$sha"
if [ "$rc $out $(cat "$wiki/Home.md")" = "1 wiki destination must be on master, not main old" ]; then ok destination_not_on_master
else bad destination_not_on_master "rc=$rc $out"; fi

src=$(source_dir empty-src notes.txt x sub/Inner.md inner .Hidden.md hidden)
wiki=$(wiki_repo empty-wiki master Home.md home)
publish "$src" "$wiki" "$sha"
if [ "$rc $out" = "1 wiki source has no Markdown pages: $src" ] && [ -f "$wiki/Home.md" ]; then ok source_has_no_pages
else bad source_has_no_pages "rc=$rc $out"; fi

wiki=$(wiki_repo self-wiki master Home.md home)
before=$(head_of "$wiki")
publish "$wiki" "$wiki" "$sha"
publish_rc=$rc publish_out=$out
publish "$wiki/" "$tmp/./self-wiki" "$sha"
if [ "$publish_rc $publish_out" = "1 wiki source and destination are the same directory: $wiki" ] && [ "$rc" = 1 ] \
  && [ "$(head_of "$wiki")" = "$before" ] && [ "$(dirty "$wiki")" = 0 ]; then ok same_directory_refused
else bad same_directory_refused "rc=$publish_rc $publish_out / rc=$rc $out"; fi

# --- mirror -----------------------------------------------------------------------------------------------------
src=$(source_dir mirror-src Home.md 'home v2' Added.md added notes.txt 'not a page' sub/Inner.md inner)
wiki=$(wiki_repo mirror-wiki master Home.md 'home v1' Gone.md gone keep.txt kept)
publish_to "$tmp/mirror-output.txt" "$src" "$wiki" "$sha"
tracked=$(fgit "$wiki" ls-files | tr '\n' ,)
got="$rc $tracked [$(dirty "$wiki")] $(fgit "$wiki" show HEAD:Home.md) $(fgit "$wiki" rev-list --count HEAD) $(fgit "$wiki" log -1 --format=%s)"
if [ "$got" = "0 Added.md,Home.md,keep.txt, [0] home v2 2 Publish wiki from 0123456" ] \
  && [ "$(cat "$tmp/mirror-output.txt")" = changed=true ] && [ "$(wc -l <"$tmp/mirror-output.txt" | tr -d ' ')" = 1 ]; then
  ok mirror_adds_changes_deletes
else bad mirror_adds_changes_deletes "$got out=$out output=$(cat "$tmp/mirror-output.txt" 2>&1)"; fi

src=$(source_dir author-src Home.md new)
wiki=$(wiki_repo author-wiki master Home.md old)
publish "$src" "$wiki" "$sha"
got="$rc $(fgit "$wiki" log -1 '--format=%an <%ae> / %cn <%ce>')"
if [ "$got" = "0 D0n9X1n <D0n9x1n@outlook.com> / D0n9X1n <D0n9x1n@outlook.com>" ]; then ok author_is_d0n9x1n
else bad author_is_d0n9x1n "$got $out"; fi

src=$(source_dir same-src Home.md home Other.md other)
wiki=$(wiki_repo same-wiki master Home.md home Other.md other)
before=$(head_of "$wiki")
publish_to "$tmp/same-output.txt" "$src" "$wiki" "$sha"
if [ "$rc $out" = "0 wiki already matches $sha" ] && [ "$(head_of "$wiki")" = "$before" ] \
  && [ "$(cat "$tmp/same-output.txt")" = changed=false ]; then ok unchanged_writes_changed_false
else bad unchanged_writes_changed_false "rc=$rc $out output=$(cat "$tmp/same-output.txt" 2>&1)"; fi

src=$(source_dir push-src Home.md new)
wiki=$(wiki_repo push-wiki master Home.md old)
git init -q --bare -b master "$tmp/push-remote.git"
fgit "$wiki" remote add origin "$tmp/push-remote.git"
fgit "$wiki" push -q origin master
seeded=$(git -C "$tmp/push-remote.git" rev-parse master)
publish "$src" "$wiki" "$sha"
if [ "$rc" = 0 ] && [ "$(git -C "$tmp/push-remote.git" rev-parse master)" = "$seeded" ] && [ "$(head_of "$wiki")" != "$seeded" ] \
  && [ -z "$out" ]; then ok no_output_file_and_no_push
else bad no_output_file_and_no_push "rc=$rc $out"; fi

# Chinese text, a trailing newline and a CR: the page arrives byte for byte.
# shellcheck disable=SC2016 # the backticks are page text
body=$(printf '# 百科\n\n侧边栏与代理：`mux-agent list`。\r\nx')
src=$(source_dir utf8-src Home-zh-CN.md "$body")
printf '\n' >>"$src/Home-zh-CN.md"
wiki=$(wiki_repo utf8-wiki master Home-zh-CN.md old)
publish "$src" "$wiki" "$sha"
if [ "$rc" = 0 ] && cmp -s "$src/Home-zh-CN.md" "$wiki/Home-zh-CN.md" && [ "$(dirty "$wiki")" = 0 ]; then ok chinese_bytes_exact
else bad chinese_bytes_exact "rc=$rc $out"; fi

# Page.md and page.md are two pages where the file system keeps them apart (Linux; macOS usually folds case).
src=$(source_dir case-src Page.md UPPER)
if [ -e "$src/page.md" ]; then skip case_distinct_names 'this file system folds case'; else
  put "$src" page.md lower
  wiki="$tmp/case-wiki"
  mkdir -p "$wiki" && git init -q -b master "$wiki"
  put "$wiki" Page.md 'old upper' && put "$wiki" page.md 'old lower'
  fgit "$wiki" add --all && fgit "$wiki" commit -q -m seed
  publish "$src" "$wiki" "$sha"
  got="$rc $(fgit "$wiki" ls-files | tr '\n' ,) $(fgit "$wiki" show HEAD:Page.md) $(fgit "$wiki" show HEAD:page.md)"
  if [ "$got" = "0 Page.md,page.md, UPPER lower" ]; then ok case_distinct_names; else bad case_distinct_names "$got $out"; fi
fi

# A page that cannot be read refuses before the destination changes.
src=$(source_dir locked-src Home.md new Other.md other)
wiki=$(wiki_repo locked-wiki master Home.md old Gone.md gone)
before=$(head_of "$wiki")
chmod 000 "$src/Home.md"
if cat "$src/Home.md" >/dev/null 2>&1; then
  skip unreadable_page_leaves_wiki 'running as a user that reads mode 000 files'
else
  publish "$src" "$wiki" "$sha"
  last=$(printf '%s\n' "$out" | tail -n 1)
  if [ "$rc $last" = "1 cannot read wiki page: Home.md" ] && [ "$(head_of "$wiki")" = "$before" ] && [ "$(dirty "$wiki")" = 0 ] \
    && [ "$(cat "$wiki/Home.md")" = old ]; then ok unreadable_page_leaves_wiki
  else bad unreadable_page_leaves_wiki "rc=$rc $out"; fi
fi
chmod 644 "$src/Home.md"

# A link could point outside wiki/, at the clone's .git/config among others: refused, nothing committed.
src=$(source_dir link-src Home.md home)
secret=$(source_dir link-secret config token=FAKE_TOKEN)
ln -s "$secret/config" "$src/Leak.md"
wiki=$(wiki_repo link-wiki master Home.md old)
before=$(head_of "$wiki")
publish "$src" "$wiki" "$sha"
if [ "$rc $out" = "1 wiki page is a link, not a file: Leak.md" ] && [ "$(head_of "$wiki")" = "$before" ] \
  && [ "$(dirty "$wiki")" = 0 ] && [ ! -e "$wiki/Leak.md" ]; then ok symlinked_page_refused
else bad symlinked_page_refused "rc=$rc $out"; fi

# The workflow passes relative paths: they resolve against the caller's directory.
base="$tmp/rel"
source_dir rel/wiki Home.md new >/dev/null
wiki_repo rel/wiki-repo master Home.md old >/dev/null
out=$(cd "$base" && sh "$publisher" wiki wiki-repo "$sha" 2>&1)
rc=$?
if [ "$rc" = 0 ] && [ "$(fgit "$base/wiki-repo" show HEAD:Home.md)" = new ] && [ "$(dirty "$base/wiki-repo")" = 0 ]; then
  ok relative_paths_resolve_against_cwd
else bad relative_paths_resolve_against_cwd "rc=$rc $out"; fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
