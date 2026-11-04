#!/usr/bin/env bash
# license: BSD-3-Clause
# program: gopls
# path: go/bin
#
# gopls v0.24.0 for linux/amd64, with the Go 1.27.3 toolchain it runs to load packages
# (binaries/go, first on gopls's PATH).
#
# The Go project publishes no gopls binaries, so gopls is compiled from the pinned module with a
# Go toolchain downloaded for the build machine; the build machine needs no Go installed. Both
# toolchains are the official distributions as served by the Go module proxy (the same zips
# `go` downloads when it switches toolchains), checked against the SHA-256 below.
# Run by build.rs with OUT (directory to fill) and MULTILSP_CACHE (download cache).
set -euo pipefail

go_version=1.27.3
target=linux-amd64
toolchains="
    darwin-arm64 8a730f886ae5c136fc467bface49a78f332fd5aeae5ad383c1f4f858bc9dd566
    darwin-amd64 dc58868b55f2797ea997f3835e242c7c5da22df94f13fb06a79da1bab1cfed08
    linux-amd64 af4c06478303fdd1bfb335fab86c7ccb9b12e529044a946a16a1c4bcc779f8d7
    linux-arm64 0ec0774cad42ecf4df82691146ee0b10c303ea594f163acb1737baedd5cc2835
"

sha256_of() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

# fetch_toolchain PLATFORM: downloads the Go toolchain zip once into the cache, verifies it, and
# prints its path.
fetch_toolchain() {
    local platform=$1 sha256 url file actual
    sha256=$(echo "$toolchains" | awk -v p="$platform" '$1 == p {print $2}')
    if [ -z "$sha256" ]; then
        echo "no Go toolchain pinned for $platform" >&2
        return 1
    fi
    url="https://proxy.golang.org/golang.org/toolchain/@v/v0.0.1-go$go_version.$platform.zip"
    mkdir -p "$MULTILSP_CACHE/downloads"
    file="$MULTILSP_CACHE/downloads/$sha256-go$go_version.$platform.zip"
    if [ ! -f "$file" ] || [ "$(sha256_of "$file")" != "$sha256" ]; then
        curl --fail --location --silent --show-error --retry 3 --output "$file.partial" "$url"
        actual=$(sha256_of "$file.partial")
        if [ "$actual" != "$sha256" ]; then
            rm -f "$file.partial"
            echo "checksum mismatch for $url: expected $sha256, got $actual" >&2
            return 1
        fi
        mv "$file.partial" "$file"
    fi
    echo "$file"
}

# unpack_toolchain ZIP PLATFORM DIR: extracts a toolchain zip as a Go root in DIR. Module zips
# carry no permissions, so the executables are marked as `go` itself does.
unpack_toolchain() {
    local zip=$1 platform=$2 dir=$3 work
    work=$(mktemp -d)
    unzip -q "$zip" -d "$work"
    mkdir -p "$dir"
    cp -R "$work/golang.org/toolchain@v0.0.1-go$go_version.$platform/." "$dir/"
    rm -rf "$work"
    chmod +x "$dir"/bin/* "$dir"/pkg/tool/*/*
}

# The toolchain shipped with gopls.
zip=$(fetch_toolchain "$target")
unpack_toolchain "$zip" "$target" "$OUT/go"

# A toolchain for this machine, to compile gopls; kept in the cache.
case "$(uname -s)" in
    Darwin) host_os=darwin ;;
    Linux) host_os=linux ;;
    *) echo "unsupported build machine: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) host_arch=arm64 ;;
    x86_64 | amd64) host_arch=amd64 ;;
    *) echo "unsupported build machine: $(uname -m)" >&2; exit 1 ;;
esac
host="$host_os-$host_arch"
host_root="$MULTILSP_CACHE/go$go_version.$host"
if [ ! -x "$host_root/bin/go" ]; then
    rm -rf "$host_root"
    zip=$(fetch_toolchain "$host")
    unpack_toolchain "$zip" "$host" "$host_root.partial"
    mv "$host_root.partial" "$host_root"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export PATH="$host_root/bin:$PATH" GOTOOLCHAIN=local GOWORK=off GOFLAGS=-modcacherw
export GOPATH="$MULTILSP_CACHE/gopath"
unset GOROOT

go mod init multilsp.local/build >/dev/null 2>&1
go get golang.org/x/tools/gopls@v0.24.0
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 \
    go build -trimpath -ldflags='-s -w' -o "$OUT/gopls" golang.org/x/tools/gopls
cp "$(go list -m -f '{{.Dir}}' golang.org/x/tools/gopls)/LICENSE" "$OUT/LICENSE"
