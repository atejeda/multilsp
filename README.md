# multilsp

Several language servers behind one LSP endpoint. The editor starts `multilsp`, and `multilsp`
starts the real language servers and routes each document to the one that handles it. See
[spec.md](spec.md) for the full specification.

## Status

Four backends, embedded in the binary for every platform that has a leaf under
[embed/](embed/README.md):

| Platform     | clangd (C, C++, Obj-C, CUDA) | gopls (Go) | rust-analyzer (Rust) | typescript (TS, JS) |
|--------------|------------------------------|------------|----------------------|---------------------|
| macOS arm64  | yes                          | yes        | yes                  | yes                 |
| macOS x86_64 | yes                          | yes        | yes                  | yes                 |
| Linux x86_64 | yes                          | yes        | yes                  | yes                 |
| Linux arm64  | -                            | yes        | yes                  | yes                 |

A backend that is not embedded runs from PATH.

Each server comes with what it needs at runtime, so it works with nothing installed:
rust-analyzer with its Rust toolchain (cargo, rustc, rust-std, rust-src), gopls with its Go
toolchain. TypeScript and JavaScript are served by TypeScript 7's native compiler
(`tsc --lsp --stdio`), which needs no Node.js. The full
bundle is large: about 150 MB compressed in the binary and 700 MB installed on macOS arm64.

## Build

Requires Rust 1.89 or newer, bash, `curl`, `tar` and `unzip`. Go is not needed: the gopls leaf
downloads a Go toolchain for the build machine to compile gopls. The first release build downloads about 500 MB and takes several minutes; later builds
reuse each leaf's `binaries/`.

```sh
cargo build --release
# binary: target/release/multilsp
```

Each technology has one leaf per version and platform:

```text
embed/<tech>/<version>/<os>-<arch>/build.sh    fills binaries/
embed/<tech>/<version>/<os>-<arch>/binaries/   embedded as-is
```

The build runs a leaf's `build.sh` only when its `binaries/` is missing: clangd's downloads the
official release and checks its pinned SHA-256; gopls's cross-compiles the pinned module, since
no gopls binaries are published, and adds the official Go toolchain. Downloads are cached in `target/bundle-cache`. See
[embed/README.md](embed/README.md) for the format and for adding versions or platforms.

| Variable                        | Effect                                           |
|---------------------------------|--------------------------------------------------|
| `MULTILSP_EMBED_<TECH>=off`     | don't embed that technology                      |
| `MULTILSP_EMBED_<TECH>=/path`   | embed that executable instead                    |
| `MULTILSP_EMBED_<TECH>_VERSION` | use that version instead of the highest          |

Every embedded executable is checked against the build target, so a wrong-platform binary
fails the build. `cargo build --release --no-default-features` embeds nothing.

## Embedded servers

On first launch `multilsp` installs the embedded servers into
`~/Library/Application Support/multilsp/versions/<bundle-id>/` (`MULTILSP_DATA_DIR` overrides
the location) and reuses that copy afterwards. Every launch checks that the files are present
with the right sizes and reinstalls if not.

```sh
multilsp bundle info     # versions, licenses, install location
multilsp bundle verify   # hash every installed file
multilsp bundle repair   # reinstall from the binary
```

`--backend clangd=/path/to/clangd` still overrides the embedded copy.

## Run

```sh
multilsp serve --stdio
multilsp serve --stdio --backend gopls=/path/to/gopls --backend clangd=/path/to/clangd
```

Logs go to stderr (stdout carries the protocol). Set `MULTILSP_LOG` to raise the level, e.g.
`MULTILSP_LOG=debug`; at `debug` the backends' own stderr is included.

## Editor setup

Neovim (0.11+):

```lua
vim.lsp.config('multilsp', {
  cmd = { 'multilsp', 'serve', '--stdio' },
  filetypes = {
    'go', 'gomod', 'gowork', 'gotmpl', 'c', 'cpp', 'objc', 'objcpp', 'cuda', 'rust',
    'typescript', 'typescriptreact', 'javascript', 'javascriptreact',
  },
  root_markers = {
    'go.work', 'go.mod', 'Cargo.toml', 'tsconfig.json', 'package.json', 'compile_commands.json', '.git',
  },
})
vim.lsp.enable('multilsp')
```

Helix (`languages.toml`):

```toml
[language-server.multilsp]
command = "multilsp"
args = ["serve", "--stdio"]

[[language]]
name = "go"
language-servers = ["multilsp"]

[[language]]
name = "c"
language-servers = ["multilsp"]

[[language]]
name = "cpp"
language-servers = ["multilsp"]

[[language]]
name = "rust"
language-servers = ["multilsp"]

[[language]]
name = "typescript"
language-servers = ["multilsp"]

[[language]]
name = "javascript"
language-servers = ["multilsp"]
```

## Tests

```sh
cargo test
```

The gateway tests use in-process fake backends and need nothing installed. `tests/servers.rs`
runs the real binary against gopls and clangd, found through `MULTILSP_GOPLS` /
`MULTILSP_CLANGD`, then `PATH`, `$GOPATH/bin` and `~/go/bin`; each test is skipped when its
servers are missing (run with `-- --nocapture` to see skips). `embedded_clangd`,
`embedded_gopls`, `embedded_rust_analyzer` and `embedded_typescript` run whichever servers the build embedded, with
nothing on PATH and exercise `bundle verify`/`repair`; tests install into `target/tmp/data`,
not your data directory.

Install gopls with `go install golang.org/x/tools/gopls@latest`; clangd comes with LLVM
(`brew install llvm`, `apt install clangd`).
