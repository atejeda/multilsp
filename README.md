# multilsp

Several language servers behind one LSP endpoint. The editor starts `multilsp`, and `multilsp`
starts the real language servers and routes each document to the one that handles it. See
[spec.md](spec.md) for the full specification.

## Status

Phase 1 of the spec: the stdio gateway, backend supervision, and routing, with **gopls** as the
only configured backend. gopls must be installed separately; embedding backends in the binary
comes in a later phase.

## Build

Requires Rust 1.88 or newer.

```sh
cargo build --release
# binary: target/release/multilsp
```

## Run

```sh
multilsp serve --stdio
multilsp serve --stdio --backend go=/path/to/gopls   # when gopls is not on PATH
```

Logs go to stderr (stdout carries the protocol). Set `MULTILSP_LOG` to raise the level, e.g.
`MULTILSP_LOG=debug`; at `debug` the backends' own stderr is included.

## Editor setup

Neovim (0.11+):

```lua
vim.lsp.config('multilsp', {
  cmd = { 'multilsp', 'serve', '--stdio' },
  filetypes = { 'go', 'gomod', 'gowork', 'gotmpl' },
  root_markers = { 'go.work', 'go.mod', '.git' },
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
```

## Tests

```sh
cargo test
```

The gateway tests use in-process fake backends and need nothing installed. `tests/gopls.rs`
runs the real binary against gopls; it looks for gopls in `MULTILSP_GOPLS`, then `PATH`,
`$GOPATH/bin` and `~/go/bin`, and is skipped if none is found.

Install gopls with `go install golang.org/x/tools/gopls@latest`.
