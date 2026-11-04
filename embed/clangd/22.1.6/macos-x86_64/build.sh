#!/usr/bin/env bash
# license: Apache-2.0 WITH LLVM-exception
# program: bin/clangd
#
# Official clangd release: the binary, its builtin headers and its license.
# Universal binary; build.rs keeps only the x86_64 slice.
source "$(dirname "$0")/../../../lib.sh"

zip=$(fetch \
    https://github.com/clangd/clangd/releases/download/22.1.6/clangd-mac-22.1.6.zip \
    631aef462556cbd74e0ebaae1778a38d1997d0ba3371652ca54f82652a179e7d)
unzip_into "$zip" clangd_22.1.6 LICENSE.TXT bin/clangd lib/clang/22/include/
