#!/usr/bin/env bash
# license: MIT OR Apache-2.0
# program: toolchain/bin/rust-analyzer
# path: toolchain/bin
# env: CARGO={dir}/toolchain/bin/cargo
# env: RUSTC={dir}/toolchain/bin/rustc
#
# rust-analyzer with the Rust 1.99.1 toolchain it needs for x86_64-apple-darwin: cargo and rustc to load
# projects and run checks, rust-std to check code, rust-src for the standard library, and
# rustc's proc-macro server. All are official components of the Rust 1.99.1 release.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

# URL and SHA-256 of each component, from the release's channel manifest.
components="
    https://static.rust-lang.org/dist/2026-10-15/rust-analyzer-1.99.1-x86_64-apple-darwin.tar.gz a572385c8ffe4c60f35405c8fef807acc9ee02b763377ede6f1c45812b1ff6f3
    https://static.rust-lang.org/dist/2026-10-15/rustc-1.99.1-x86_64-apple-darwin.tar.gz dad7c1300549bc846e1f02652fa11d131b86cdbb8288d64da5f62b4504d62a8f
    https://static.rust-lang.org/dist/2026-10-15/cargo-1.99.1-x86_64-apple-darwin.tar.gz ed05dab7a703d1b4c6fa5cec8e99272cb1c3902a323986c80e0bc57915c85ddd
    https://static.rust-lang.org/dist/2026-10-15/rust-std-1.99.1-x86_64-apple-darwin.tar.gz 5cc4606fdd6c9226be51f59a333942cc8a203e74f7da38d47aaaf44b986bfe3c
    https://static.rust-lang.org/dist/2026-10-15/rust-src-1.99.1.tar.gz b0d6d3a7e46a97adfe6488f8b60c04c019566b8363eeb6ffb32c6cf63609071d
"

sha256_of() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$MULTILSP_CACHE/downloads" "$OUT/toolchain"

echo "$components" | while read -r url sha256; do
    [ -n "$url" ] || continue
    # Download once into the cache and verify.
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
    # Each tarball holds <name>/<component>/{bin,lib,libexec,...}: merge the components into
    # one toolchain, the way rustup lays it out.
    rm -rf "$work/unpack" && mkdir "$work/unpack"
    tar -xzf "$file" -C "$work/unpack"
    for component in "$work"/unpack/*/*/; do
        cp -R "$component." "$OUT/toolchain/"
    done
done

# Installer metadata, docs and shell completions are not needed.
rm -rf "$OUT/toolchain/manifest.in" "$OUT/toolchain/share" "$OUT/toolchain/etc"
