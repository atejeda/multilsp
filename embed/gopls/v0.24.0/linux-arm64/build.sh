#!/usr/bin/env bash
# license: BSD-3-Clause
# program: gopls
#
# gopls v0.24.0 for linux/arm64. The Go project publishes no gopls binaries, so this
# cross-compiles the pinned module; Go verifies it against its checksum database. Needs Go on
# the build machine. Run by build.rs with OUT (directory to fill).
set -euo pipefail

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export GOFLAGS=-modcacherw GOWORK=off

go mod init multilsp.local/build >/dev/null 2>&1
go get golang.org/x/tools/gopls@v0.24.0
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 \
    go build -trimpath -ldflags='-s -w' -o "$OUT/gopls" golang.org/x/tools/gopls
cp "$(go list -m -f '{{.Dir}}' golang.org/x/tools/gopls)/LICENSE" "$OUT/LICENSE"
