#!/usr/bin/env bash
# license: MIT
# program: ty
# args: server
#
# ty 0.0.97 for aarch64-unknown-linux-gnu: Astral's Python type checker and language server (`ty server`). A single
# native binary; it carries typeshed's stubs, so the standard library works without a Python
# interpreter. Official release binary, plus the license at the same tag.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

files="
    https://github.com/astral-sh/ty/releases/download/0.0.97/ty-aarch64-unknown-linux-gnu.tar.gz a369a6af123af9a1bc8883d03a4f91a7cdd88f98758d59123e3aacd4fae58f93
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
tar -xzf "$downloads/a369a6af123af9a1bc8883d03a4f91a7cdd88f98758d59123e3aacd4fae58f93-ty-aarch64-unknown-linux-gnu.tar.gz" -C "$OUT" --strip-components=1 ty-aarch64-unknown-linux-gnu/ty
cp "$downloads/860e3d7a86b84e6a7012c7a635fc64df475cebc6cce34dfeb73a5982ec58176c-LICENSE" "$OUT/LICENSE"
chmod +x "$OUT/ty"
