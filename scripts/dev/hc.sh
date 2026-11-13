#!/bin/sh
set -eu
: "${RUNNER_TEMP:?}" "${GITHUB_PATH:?}"
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target=linux-x86_64; sha=78424755c10a40dde38a0bdb55c20ac19706e9ae05ed68fc2acf1fcff0d941c1 ;;
  Darwin:arm64|Darwin:aarch64) target=macos-aarch64; sha=1ab38dd9270f63a8f794d6d60e8702eb0d1c85a72bda85d735e5eea43f05ffed ;;
  Darwin:x86_64) target=macos-x86_64; sha=e377e1d342cddce0346da5b2f3c281b9701e4731c00024c7c5028265e77a217f ;;
  MINGW*:x86_64|MSYS*:x86_64) target=windows-x86_64; sha=a8984634c35b58ba82d0aec078d5a5e6004da01d5a25b3daf822ab5e31c7312b ;;
  *) exit 2 ;;
esac
ext=tar.gz; exe=herdr-core
case "$target" in windows-*) ext=zip; exe=herdr-core.exe ;; esac
temp=$RUNNER_TEMP; case "$target" in windows-*) temp=$(cygpath -u "$temp") ;; esac
download=$(mktemp -d "$temp/hc-download.XXXXXX")
bundle=$(mktemp -d "$temp/hc-bundle.XXXXXX")
trap 'rm -rf "$download"' EXIT HUP INT TERM
curl --fail --silent --show-error --location "https://releases.herdr.dev/core/0.1.21/herdr-core-$target.$ext" -o "$download/core.$ext"
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$download/core.$ext" | cut -d ' ' -f 1)
else got=$(shasum -a 256 "$download/core.$ext" | cut -d ' ' -f 1); fi
[ "$got" = "$sha" ]
if [ "$target" = windows-x86_64 ]; then
  native_tar="$(cygpath -u "$SYSTEMROOT")/System32/tar.exe"
  "$native_tar" -xf "$(cygpath -w "$download/core.$ext")" -C "$(cygpath -w "$bundle")"
  printf '%s\n' "$(cygpath -w "$bundle")" >> "$GITHUB_PATH"
else
  tar -xf "$download/core.$ext" -C "$bundle"
  printf '%s\n' "$bundle" >> "$GITHUB_PATH"
fi
[ "$("$bundle/$exe" --version | tr -d '\r')" = 'hc 0.1.21' ]
