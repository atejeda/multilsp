#!/usr/bin/env bash
# license: MIT OR Apache-2.0
# program: toolchain/bin/rust-analyzer
# path: toolchain/bin
# env: CARGO={dir}/toolchain/bin/cargo
# env: RUSTC={dir}/toolchain/bin/rustc
#
# rust-analyzer with the Rust 1.99.1 toolchain it needs for aarch64-apple-darwin: cargo and rustc to load
# projects and run checks, rust-std to check code, rust-src for the standard library, and
# rustc's proc-macro server. All are official components of the Rust 1.99.1 release.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

# URL and SHA-256 of each component, from the release's channel manifest.
components="
    https://static.rust-lang.org/dist/2026-10-15/rust-analyzer-1.99.1-aarch64-apple-darwin.tar.gz 4381c189504807bdb4b92e2d151ffb168f44e1d5eed35c8addb96184a682a12d
    https://static.rust-lang.org/dist/2026-10-15/rustc-1.99.1-aarch64-apple-darwin.tar.gz cc998c1dc8df006106472357aa87838b2da3f7b25446b98a94cfd58796cb0610
    https://static.rust-lang.org/dist/2026-10-15/cargo-1.99.1-aarch64-apple-darwin.tar.gz b5cc7634f919949c3e5e131123f813416dc47f6b809a5371d12f1f593595ae13
    https://static.rust-lang.org/dist/2026-10-15/rust-std-1.99.1-aarch64-apple-darwin.tar.gz c124006d63885ba1029aca49efd59e93be6b2b171c379318ff94a00ef5eba664
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
