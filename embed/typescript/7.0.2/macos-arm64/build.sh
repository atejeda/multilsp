#!/usr/bin/env bash
# license: Apache-2.0
# program: lib/tsc
# args: --lsp --stdio
#
# TypeScript 7.0.2 for darwin-arm64: the native compiler, which includes the language server for
# TypeScript and JavaScript (`tsc --lsp --stdio`), and the lib.*.d.ts files it loads. It needs
# no Node.js. This is the official @typescript/typescript-darwin-arm64 package from the npm registry.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

url=https://registry.npmjs.org/@typescript/typescript-darwin-arm64/-/typescript-darwin-arm64-7.0.2.tgz
sha256=902e2fe1cf0799198ef902c6b8c310a450fef629a6baba41d45641ef75c04ebd

sha256_of() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

# Download once into the cache and verify.
mkdir -p "$MULTILSP_CACHE/downloads"
tgz="$MULTILSP_CACHE/downloads/$sha256-${url##*/}"
if [ ! -f "$tgz" ] || [ "$(sha256_of "$tgz")" != "$sha256" ]; then
    curl --fail --location --silent --show-error --retry 3 --output "$tgz.partial" "$url"
    actual=$(sha256_of "$tgz.partial")
    if [ "$actual" != "$sha256" ]; then
        rm -f "$tgz.partial"
        echo "checksum mismatch for $url: expected $sha256, got $actual" >&2
        exit 1
    fi
    mv "$tgz.partial" "$tgz"
fi

# The package is package/{lib/tsc,lib/lib.*.d.ts,LICENSE,NOTICE.txt,...}.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xzf "$tgz" -C "$work"
cp -R "$work/package/lib" "$OUT/lib"
cp "$work/package/LICENSE" "$work/package/NOTICE.txt" "$OUT/"
chmod +x "$OUT/lib/tsc"
