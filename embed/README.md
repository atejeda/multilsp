# Embedded language servers

```text
embed/
  lib.sh                                  helpers the scripts source
  <tech>/<version>/<os>-<arch>/build.sh   puts the files to embed into $OUT
  <tech>/<version>/<os>-<arch>/binaries/  the result; this is what gets embedded
```

- `<tech>` is the backend name (`clangd`, `gopls`), as used by `--backend` and `bundle info`.
- `<os>-<arch>` is `macos-arm64`, `macos-x86_64`, `linux-x86_64` or `linux-arm64`, derived from
  the Rust build target.
- `<version>` is free-form. The highest version that has a leaf for the target wins (compared
  numerically, a leading `v` ignored) unless `MULTILSP_EMBED_<TECH>_VERSION` names another.

A target with no leaf gets no embedded copy of that technology; it runs from PATH.

## How a leaf is used

`cargo build` embeds `binaries/` as-is when it exists, so binaries can also be placed there by
hand. Otherwise it runs `bash build.sh` first, with:

| Variable            | Meaning                                                          |
|---------------------|------------------------------------------------------------------|
| `OUT`               | empty directory to fill; renamed to `binaries/` if the script succeeds |
| `MULTILSP_CACHE`    | cache shared across builds (`target/bundle-cache`)               |
| `MULTILSP_TARGET`   | Rust target triple                                               |
| `MULTILSP_PLATFORM` | the leaf's `<os>-<arch>`                                         |

Delete `binaries/` to run the script again. `binaries/` is not committed.

The script's header declares what it produces:

```bash
# license: BSD-3-Clause      SPDX expression, shown by `multilsp bundle info`
# program: gopls             the executable, relative to binaries/
```

Without `# program:`, `<tech>` or `bin/<tech>` is used.

## lib.sh

| Function                                       | Does                                                         |
|------------------------------------------------|--------------------------------------------------------------|
| `fetch URL SHA256`                             | downloads once into the cache, fails on a hash mismatch, prints the path |
| `unzip_into ZIP PREFIX PATH...`                | extracts `PREFIX/PATH` entries into `$OUT` (`dir/` takes a directory) |
| `go_build MODULE VERSION GOOS GOARCH NAME`     | cross-compiles a Go module into `$OUT/NAME`, with its LICENSE |

## Overrides

| Variable                        | Effect                                           |
|---------------------------------|--------------------------------------------------|
| `MULTILSP_EMBED_<TECH>=off`     | skip that technology                             |
| `MULTILSP_EMBED_<TECH>=/path`   | package that executable instead of the leaf      |
| `MULTILSP_EMBED_<TECH>_VERSION` | use that version instead of the highest          |

Every embedded executable is checked against the build target (ELF or Mach-O, CPU); a
wrong-platform binary fails the build. macOS universal binaries are thinned to the target's
slice.

## Adding a version or platform

Copy the nearest leaf to `embed/<tech>/<new-version>/<os>-<arch>/build.sh` and edit it. For a
download, get the hash with `shasum -a 256 <file>`.
