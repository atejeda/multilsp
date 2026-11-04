#!/usr/bin/env bash
# license: BSD-3-Clause
# program: gopls
#
# The Go project publishes no gopls binaries, so this cross-compiles the pinned module. Needs Go
# on the build machine.
source "$(dirname "$0")/../../../lib.sh"

go_build golang.org/x/tools/gopls v0.24.0 linux arm64 gopls
