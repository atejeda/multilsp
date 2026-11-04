#!/usr/bin/env bash
# license: Apache-2.0 WITH LLVM-exception
# program: bin/clangd
#
# Official clangd 22.1.6 release for macOS (universal; build.rs keeps the x86_64 slice): the binary, its builtin headers and its license.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

url=https://github.com/clangd/clangd/releases/download/22.1.6/clangd-mac-22.1.6.zip
sha256=631aef462556cbd74e0ebaae1778a38d1997d0ba3371652ca54f82652a179e7d

sha256_of() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

# Download once into the cache and verify.
mkdir -p "$MULTILSP_CACHE/downloads"
zip="$MULTILSP_CACHE/downloads/$sha256-clangd-mac-22.1.6.zip"
if [ ! -f "$zip" ] || [ "$(sha256_of "$zip")" != "$sha256" ]; then
    curl --fail --location --silent --show-error --retry 3 --output "$zip.partial" "$url"
    actual=$(sha256_of "$zip.partial")
    if [ "$actual" != "$sha256" ]; then
        rm -f "$zip.partial"
        echo "checksum mismatch for $url: expected $sha256, got $actual" >&2
        exit 1
    fi
    mv "$zip.partial" "$zip"
fi

# Keep only what clangd needs.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
unzip -q "$zip" \
    "clangd_22.1.6/LICENSE.TXT" \
    "clangd_22.1.6/bin/clangd" \
    "clangd_22.1.6/lib/clang/22/include/*" \
    -d "$work"
cp -R "$work/clangd_22.1.6/." "$OUT/"
