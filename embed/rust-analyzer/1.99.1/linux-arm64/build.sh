#!/usr/bin/env bash
# license: MIT OR Apache-2.0
# program: toolchain/bin/rust-analyzer
# path: toolchain/bin
# env: CARGO={dir}/toolchain/bin/cargo
# env: RUSTC={dir}/toolchain/bin/rustc
#
# rust-analyzer with the Rust 1.99.1 toolchain it needs for aarch64-unknown-linux-gnu: cargo and rustc to load
# projects and run checks, rust-std to check code, rust-src for the standard library, and
# rustc's proc-macro server. All are official components of the Rust 1.99.1 release.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

# URL and SHA-256 of each component, from the release's channel manifest.
components="
    https://static.rust-lang.org/dist/2026-10-15/rust-analyzer-1.99.1-aarch64-unknown-linux-gnu.tar.gz dd3e70441271a4901fe15a7378b66968217e3ffe624e0b3a3ead1ce70d3a10a6
    https://static.rust-lang.org/dist/2026-10-15/rustc-1.99.1-aarch64-unknown-linux-gnu.tar.gz e4656174f0ba5ea3dab5f58d069b7afae6d7616187de31c65cb6432e4f941399
    https://static.rust-lang.org/dist/2026-10-15/cargo-1.99.1-aarch64-unknown-linux-gnu.tar.gz 201dbeac1795713b98aeae044e01695e70ae2ea3ded42683ebce5a8a1c90f8a6
    https://static.rust-lang.org/dist/2026-10-15/rust-std-1.99.1-aarch64-unknown-linux-gnu.tar.gz 32d1ddb44cd49d6497ab6a24f440d4286a20b2667fdf928b876f59c66d13d722
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
