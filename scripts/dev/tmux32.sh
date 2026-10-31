#!/bin/sh
# Builds tmux 3.2a, the oldest tmux mux-agent supports, for the live tests: target/tmux-3.2a/bin/tmux.
# Reuses a build already there. The tarball goes into its own new empty directory and its SHA-256 is checked before
# anything reads it; source, objects and install all stay under target/tmux-3.2a. Last line on stdout:
#   MUX_AGENTS_TMUX32=<absolute path of the binary>
# Needs curl, a C compiler, make, pkg-config and libevent (macOS: brew install libevent pkg-config).
set -eu

version=3.2a
url="https://github.com/tmux/tmux/releases/download/$version/tmux-$version.tar.gz"
sha256=551553a4f82beaa8dadc9256800bcc284d7c000081e47aa6ecbb6ff36eacd05f

here=$(cd "$(dirname "$0")" && pwd -P)
repo=$(cd "$here/../.." && pwd -P)
out="$repo/target/tmux-$version"
bin="$out/bin/tmux"

if [ -x "$bin" ] && [ "$("$bin" -V 2>/dev/null)" = "tmux $version" ]; then
    echo "MUX_AGENTS_TMUX32=$bin"
    exit 0
fi

mkdir -p "$out"
rm -rf "$out/download" "$out/src" "$out/bin" "$out/share"
mkdir "$out/download" "$out/src"
tarball="$out/download/tmux-$version.tar.gz"

echo "downloading $url" >&2
curl -fsSL --proto '=https' -o "$tarball" "$url"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tarball" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$tarball" | cut -d ' ' -f 1)
fi
if [ "$actual" != "$sha256" ]; then
    echo "tmux32.sh: SHA-256 mismatch for $tarball: got $actual, want $sha256" >&2
    rm -f "$tarball"
    exit 1
fi

tar -xzf "$tarball" -C "$out/src"
src="$out/src/tmux-$version"

if [ "$(uname -s)" = Darwin ] && command -v brew >/dev/null 2>&1; then
    PKG_CONFIG_PATH="$(brew --prefix libevent)/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export PKG_CONFIG_PATH
fi

echo "building tmux $version in $src" >&2
(
    cd "$src"
    # tmux 3.2a predates the C99-strict defaults of current compilers.
    CFLAGS="${CFLAGS:-} -Wno-implicit-function-declaration -Wno-int-conversion" \
        ./configure --prefix="$out" --disable-utf8proc >&2
    make -j "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)" >&2
    make install >&2
)

test "$("$bin" -V)" = "tmux $version"
echo "MUX_AGENTS_TMUX32=$bin"
