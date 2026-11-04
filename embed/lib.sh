# Helpers for embed/<tech>/<version>/<os>-<arch>/build.sh scripts. Portable to the bash 3.2 that
# macOS ships.
#
# build.rs runs each script with:
#   OUT                where to put the files to embed (renamed to binaries/ on success)
#   MULTILSP_CACHE     download and build cache shared across builds
#   MULTILSP_TARGET    Rust target triple, e.g. aarch64-apple-darwin
#   MULTILSP_PLATFORM  leaf name, e.g. macos-arm64

set -euo pipefail

sha256_of() {
    if command -v shasum >/dev/null; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}

# fetch URL SHA256: downloads into the cache once, verifies, and prints the file's path.
fetch() {
    local url=$1 expected=$2
    local dir="$MULTILSP_CACHE/downloads"
    local file="$dir/$expected-${url##*/}"
    mkdir -p "$dir"
    if [ ! -f "$file" ] || [ "$(sha256_of "$file")" != "$expected" ]; then
        curl --fail --location --silent --show-error --retry 3 --output "$file.partial" "$url"
        local actual
        actual=$(sha256_of "$file.partial")
        if [ "$actual" != "$expected" ]; then
            rm -f "$file.partial"
            echo "checksum mismatch for $url: expected $expected, got $actual" >&2
            return 1
        fi
        mv "$file.partial" "$file"
    fi
    echo "$file"
}

# unzip_into ZIP PREFIX PATH...: extracts PREFIX/PATH entries into $OUT as PATH. A PATH ending
# in / takes the whole directory.
unzip_into() {
    local zip=$1 prefix=$2
    shift 2
    local work patterns=() path
    work=$(mktemp -d)
    for path in "$@"; do
        case $path in
            */) patterns+=("$prefix/$path*") ;;
            *) patterns+=("$prefix/$path") ;;
        esac
    done
    unzip -q "$zip" "${patterns[@]}" -d "$work"
    cp -R "$work/$prefix/." "$OUT/"
    rm -rf "$work"
}

# go_build MODULE VERSION GOOS GOARCH NAME: cross-compiles MODULE@VERSION into $OUT/NAME, with
# the module's LICENSE. Go verifies the module against its checksum database.
go_build() {
    local module=$1 version=$2 goos=$3 goarch=$4 name=$5
    local work
    work=$(mktemp -d)
    (
        cd "$work"
        export GOFLAGS=-modcacherw GOWORK=off
        go mod init multilsp.local/build >/dev/null 2>&1
        go get "$module@$version"
        GOOS=$goos GOARCH=$goarch CGO_ENABLED=0 \
            go build -trimpath -ldflags='-s -w' -o "$OUT/$name" "$module"
        cp "$(go list -m -f '{{.Dir}}' "$module")/LICENSE" "$OUT/LICENSE"
    )
    rm -rf "$work"
}
