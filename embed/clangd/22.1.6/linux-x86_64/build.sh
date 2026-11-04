#!/usr/bin/env bash
# license: Apache-2.0 WITH LLVM-exception
# program: bin/clangd
#
# Official clangd release: the binary, its builtin headers and its license.
# Linux x86_64 build from the clangd project.
source "$(dirname "$0")/../../../lib.sh"

zip=$(fetch \
    https://github.com/clangd/clangd/releases/download/22.1.6/clangd-linux-22.1.6.zip \
    a9c77443af2e447ed467e84771848d3a6ac1c56f84bcfcde717e66318de77cfa)
unzip_into "$zip" clangd_22.1.6 LICENSE.TXT bin/clangd lib/clang/22/include/
