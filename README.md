# multilsp

Several language servers behind one LSP endpoint. The editor starts `multilsp`, and `multilsp`
starts the real language servers and routes each document to the one that handles it. See
[spec.md](spec.md) for the full specification.

## Status

The stdio gateway, backend supervision, routing, and capability reconciliation, with two
backends: **gopls** (Go) and **clangd** (C, C++, Objective-C, CUDA). The servers must be installed
separately; embedding them in the binary comes in a later phase. A backend that is not installed
is skipped, so either one alone works.

## Build

Requires Rust 1.88 or newer.

```sh
cargo build --release
# binary: target/release/multilsp
```

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
servers are missing (run with `-- --nocapture` to see skips).

Install gopls with `go install golang.org/x/tools/gopls@latest`; clangd comes with LLVM
(`brew install llvm`, `apt install clangd`).
