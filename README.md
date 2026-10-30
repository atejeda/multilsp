# multilsp

Several language servers behind one LSP endpoint. The editor starts `multilsp`, and `multilsp`
starts the real language servers and routes each document to the one that handles it. See
[spec.md](spec.md) for the full specification.

## Status

Two backends: **clangd** (C, C++, Objective-C, CUDA) and **gopls** (Go).

On **macOS with Apple silicon** (`aarch64-apple-darwin`), clangd is embedded in the binary and
needs nothing installed. gopls is not embedded yet and runs from PATH. On other targets
nothing is embedded and both run from PATH.

## Build

Requires Rust 1.89 or newer and `curl`.

```sh
cargo build --release
# binary: target/release/multilsp
```

The build downloads the official clangd release pinned in [bundle/sources.json](bundle/sources.json),
checks it against the pinned SHA-256, keeps the `clangd` binary (arm64 slice only), its builtin
headers and its license, and embeds them compressed (about 18 MB). Nothing is compiled from
source. Downloads are cached in `target/bundle-cache` (override with `MULTILSP_BUNDLE_CACHE`), so
later builds work offline.

`cargo build --release --no-default-features` builds without embedded servers.

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
  filetypes = { 'go', 'gomod', 'gowork', 'gotmpl', 'c', 'cpp', 'objc', 'objcpp', 'cuda' },
  root_markers = { 'go.work', 'go.mod', 'compile_commands.json', '.git' },
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
```

## Tests

```sh
cargo test
```

The gateway tests use in-process fake backends and need nothing installed. `tests/servers.rs`
runs the real binary against gopls and clangd, found through `MULTILSP_GOPLS` /
`MULTILSP_CLANGD`, then `PATH`, `$GOPATH/bin` and `~/go/bin`; each test is skipped when its
servers are missing (run with `-- --nocapture` to see skips). On Apple silicon,
`embedded_clangd` runs the embedded clangd with an empty PATH and exercises
`bundle verify`/`repair`; tests install into `target/tmp/data`, not your data directory.

Install gopls with `go install golang.org/x/tools/gopls@latest`; clangd comes with LLVM
(`brew install llvm`, `apt install clangd`).
