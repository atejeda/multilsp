#!/usr/bin/env bash
# license: MIT
# program: ty
# args: server
#
# ty 0.0.97 for x86_64-unknown-linux-gnu: Astral's Python type checker and language server (`ty server`). A single
# native binary; it carries typeshed's stubs, so the standard library works without a Python
# interpreter. Official release binary, plus the license at the same tag.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

files="
    https://github.com/astral-sh/ty/releases/download/0.0.97/ty-x86_64-unknown-linux-gnu.tar.gz d1d257df052ac5dc5c6a114f7fc73a5ff4253e0d4f714095b6c8772cfd8f1bf4
    https://raw.githubusercontent.com/astral-sh/ty/0.0.97/LICENSE 860e3d7a86b84e6a7012c7a635fc64df475cebc6cce34dfeb73a5982ec58176c
"

sha256_of() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

# Download each file once into the cache and verify.
mkdir -p "$MULTILSP_CACHE/downloads"
echo "$files" | while read -r url sha256; do
    [ -n "$url" ] || continue
    file="$MULTILSP_CACHE/downloads/$sha256-${url##*/}"
    if [ ! -f "$file" ] || [ "$(sha256_of "$file")" != "$sha256" ]; then
        curl --fail --location --silent --show-error --retry 3 --output "$file.partial" "$url"
        actual=$(sha256_of "$file.partial")
        if [ "$actual" != "$sha256" ]; then
            rm -f "$file.partial"
            echo "checksum mismatch for $url: expected $sha256, got $actual" >&2
            exit 1
        fi
        mv "$file.partial" "$file"
    fi
done

downloads="$MULTILSP_CACHE/downloads"
tar -xzf "$downloads/d1d257df052ac5dc5c6a114f7fc73a5ff4253e0d4f714095b6c8772cfd8f1bf4-ty-x86_64-unknown-linux-gnu.tar.gz" -C "$OUT" --strip-components=1 ty-x86_64-unknown-linux-gnu/ty
cp "$downloads/860e3d7a86b84e6a7012c7a635fc64df475cebc6cce34dfeb73a5982ec58176c-LICENSE" "$OUT/LICENSE"
chmod +x "$OUT/ty"
