#!/bin/sh
# Mirrors the top-level *.md pages of <source-dir> into the wiki worktree <wiki-repo> (branch master) and commits them
# as D0n9X1n. Never pushes. Appends changed=true|false to $GITHUB_OUTPUT when set.
#   publish.sh <source-dir> <wiki-repo> <source-sha>
# Relative paths resolve against the current directory. Exit 0 done, 1 refused or git failed, 2 usage. POSIX sh and git.
set -u

refuse() {
  echo "$*" >&2
  exit 1
}

output() {
  if [ -n "${GITHUB_OUTPUT-}" ]; then printf '%s\n' "$1" >>"$GITHUB_OUTPUT"; fi
}

if [ "$#" -ne 3 ] || [ -z "$1" ] || [ -z "$2" ] || [ -z "$3" ]; then
  echo 'usage: publish.sh <source-dir> <wiki-repo> <source-sha>' >&2
  exit 2
fi
case $1 in /*) source=$1 ;; *) source=$PWD/$1 ;; esac
case $2 in /*) wiki=$2 ;; *) wiki=$PWD/$2 ;; esac
sha=$3

[ -d "$source" ] || refuse "wiki source is not a directory: $source"
[ -e "$wiki/.git" ] || refuse "wiki destination is not a git worktree: $wiki"
# -ef compares the folders themselves, so a trailing slash, a ./ or a case-folding file system cannot hide a match.
if [ "$source" -ef "$wiki" ]; then refuse "wiki source and destination are the same directory: $wiki"; fi
branch=$(git -C "$wiki" branch --show-current)
[ "$branch" = master ] || refuse "wiki destination must be on master, not $branch"

# Every page is read before the destination changes, so a page that cannot be read leaves the wiki as it was.
# A link could point outside wiki/, at the clone's .git/config among others, so links are refused.
# Pages are staged under numbers, not names: a case-folding temp folder would merge Page.md and page.md, which are
# two pages where the source file system says so. Hidden pages (.*.md) are not pages.
stage=$(mktemp -d) || refuse 'cannot make a staging folder'
trap 'rm -rf "$stage"' EXIT
set --
for page in "$source"/*.md; do
  name=${page##*/}
  if [ -L "$page" ]; then refuse "wiki page is a link, not a file: $name"; fi
  [ -f "$page" ] || continue
  cat -- "$page" >"$stage/$(($# + 1))" 2>/dev/null || refuse "cannot read wiki page: $name"
  set -- "$@" "$name"
done
[ "$#" -gt 0 ] || refuse "wiki source has no Markdown pages: $source"

# Every .md entry of the destination goes, hidden ones and links too, so no write follows a link out of the clone.
for old in "$wiki"/*.md "$wiki"/.*md; do
  case $old in *.md) ;; *) continue ;; esac
  if [ -f "$old" ] || [ -L "$old" ]; then
    err=$(rm -f -- "$old" 2>&1) || refuse "cannot write the wiki destination: $err"
  fi
done
# Written, not copied: a copy keeps the source mtime, and git would take a same-size change in the same second for
# unchanged.
i=0
for name in "$@"; do
  i=$((i + 1))
  err=$(cat -- "$stage/$i" 2>&1 >"$wiki/$name") || refuse "cannot write the wiki destination: $err"
done

git -C "$wiki" add --all || refuse 'git add failed in the wiki destination'
if git -C "$wiki" diff --cached --quiet; then
  output changed=false
  echo "wiki already matches $sha"
  exit 0
fi
git -C "$wiki" -c user.name=D0n9X1n -c user.email=D0n9x1n@outlook.com commit -q -m "Publish wiki from $(printf '%.7s' "$sha")" \
  || refuse 'git commit failed in the wiki destination'
output changed=true
exit 0
