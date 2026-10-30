# multilsp — Technical Specification

**Status:** Draft v0.1  
**Date:** 2026-10-30  
**Implementation language:** Rust  
**Project / public crate / executable:** `multilsp`

## 1. Overview

`multilsp` is a Rust library and standalone Language Server Protocol (LSP) server that presents a single LSP endpoint while orchestrating existing language-server implementations. Its standalone distribution is a **single self-contained executable per supported OS/architecture**. The executable embeds a verified bundle of backend executables, runtimes, and resources. On first use (or when an installed bundle is missing or invalid), it installs those resources into an application-managed directory and launches the required backends as subprocesses.

The public Rust library exposes the same orchestration functionality to host applications, without requiring them to launch the `multilsp` CLI.

## 2. Goals and scope

### Goals

- Present one standards-compliant LSP endpoint to editor clients.
- Route requests and notifications to the appropriate underlying language server.
- Support Go, C/C++, TypeScript/JavaScript, Python, and Rust initially.
- Ship one distributable `multilsp` executable for each supported target.
- Build and package all required backend components automatically.
- Install embedded components once, reuse them, and repair invalid installations.
- Provide a reusable, documented Rust API alongside the CLI.
- Operate offline after compilation, including on first launch.

### Initial supported targets

| OS | Architecture | Rust target triple |
|---|---|---|
| Linux | x86_64 | `x86_64-unknown-linux-gnu` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` |
| macOS | x86_64 | `x86_64-apple-darwin` |
| macOS | ARM64 | `aarch64-apple-darwin` |

Windows and other architectures are outside the initial scope. Minimum OS, libc, and system-library versions remain to be defined and tested. A single executable means **one downloadable file per target**, not one universal binary across all platforms.

## 3. Language-server backends

| Language | Initial backend candidate | Notes |
|---|---|---|
| Go | `gopls` | Pin and package target-compatible binary. |
| C/C++ | `clangd` | Include required runtime resources and account for native dependencies. |
| TypeScript / JavaScript | `typescript-language-server` | Bundle compatible Node.js runtime and JS packages, or select an equivalent standalone solution. |
| Python | Pyright or another suitable LSP | Choose backend and runtime strategy during implementation design. |
| Rust | `rust-analyzer` | Pin and package target-compatible binary. |

Backend selection is configurable in the architecture. The table specifies initial integration candidates, not a promise that all backends are already available as portable standalone upstream artifacts.

## 4. Functional requirements

### 4.1 LSP gateway

- **FR-001:** Accept an LSP client connection using JSON-RPC 2.0 over stdio in the initial CLI implementation.
- **FR-002:** Implement LSP initialization and shutdown semantics and manage downstream backend initialization.
- **FR-003:** Route document-scoped requests and notifications by language and document URI.
- **FR-004:** Forward backend results, errors, diagnostics, progress, and applicable notifications to the client.
- **FR-005:** Preserve request/response correlation when forwarding to multiple backend processes.
- **FR-006:** Handle cancellation, backend failures, and timeouts without corrupting the client protocol stream.
- **FR-007:** Advertise only capabilities that can actually be fulfilled; reconcile client and backend capabilities.
- **FR-008:** Support workspace folders and changes to document lifecycle (`didOpen`, `didChange`, `didSave`, `didClose`).
- **FR-009:** Support multiple concurrent backend processes and multiple active languages in a workspace.
- **FR-010:** Avoid writing diagnostic logs to stdout while stdio is being used for LSP messages.

**Routing rules requiring detailed design:** workspace-wide requests, file types handled by multiple backends, merging completion results, diagnostic ownership, dynamic capability registration, URI rewriting (if needed), and multi-root workspace behavior.

### 4.2 Backend lifecycle

- **FR-011:** Start backend processes when needed, with configured arguments, environment, and working directory.
- **FR-012:** Track process health and exit status; surface actionable failures.
- **FR-013:** Shut down backend processes gracefully, then forcibly terminate if necessary.
- **FR-014:** Provide a backend registry with language mappings and executable/resource locations.
- **FR-015:** Allow configurable external backend paths as an alternative to embedded components.

### 4.3 Packaging and installation

- **FR-016:** The default release build must embed every required backend executable, runtime, library, and resource for its target.
- **FR-017:** Runtime startup must inspect the installed bundle before extracting it.
- **FR-018:** Extract only if the matching installation is absent, incomplete, or invalid.
- **FR-019:** Use an application-managed persistent data directory, honoring platform conventions.
- **FR-020:** Store installations in versioned or content-addressed directories so upgrades and downgrades do not conflict.
- **FR-021:** Validate packaged and extracted files against a manifest containing checksums.
- **FR-022:** Perform installation with a cross-process lock, temporary staging directory, and atomic publication where supported.
- **FR-023:** Recover automatically from interrupted installation, missing files, or detected corruption.
- **FR-024:** Permit concurrent `multilsp` instances to safely reuse a completed installation.
- **FR-025:** Require no network access when executing a fully bundled release, including first-run extraction.
- **FR-026:** Do not silently replace a valid installed bundle with unverified downloaded code.

### 4.4 Library and executable

- **FR-027:** Publish a public Rust library named `multilsp` with a documented API for configuration and backend orchestration.
- **FR-028:** Produce a standalone executable named `multilsp` that uses the same core library implementation.
- **FR-029:** Permit library consumers to initialize, start, stop, and inspect backend instances directly.
- **FR-030:** Keep transport/CLI concerns separate from the core orchestration API.
- **FR-031:** Provide asynchronous operations where appropriate; Tokio is the initial runtime candidate.
- **FR-032:** Support an opt-out build without embedded backends for library users and advanced deployments.

## 5. Build and distribution requirements

- **BR-001:** A standard release build (`cargo build --release`) must automatically prepare and embed the target's complete backend bundle by default.
- **BR-002:** The build system must fetch prebuilt artifacts or compile dependencies from pinned source versions as needed.
- **BR-003:** Resolve backend artifacts for the **build target**, not blindly for the build host.
- **BR-004:** Keep a declarative dependency manifest and lock information with exact versions, sources, checksums, and license metadata.
- **BR-005:** Verify all acquired artifacts before inclusion.
- **BR-006:** Cache verified downloads and intermediate outputs to avoid repeated work.
- **BR-007:** Fail clearly if any required backend, runtime, or resource cannot be acquired or packaged.
- **BR-008:** Compress the bundle when practical and embed it in the final executable.
- **BR-009:** Produce automated CI release artifacts for all four initial targets.
- **BR-010:** Support a core-only build (proposed: `cargo build --release --no-default-features`).
- **BR-011:** Document licenses and redistribute third-party notices for bundled components.

**Build orchestration:** packaging may be implemented as a dedicated tool invoked from Cargo's `build.rs` or a coordinated build pipeline, but the standard user-facing build command must remain sufficient. Build scripts must avoid unnecessary network activity and support repeatable cached builds. A clean build requires network access unless all dependency sources are already available locally.

## 6. Proposed architecture

```text
                    Editor / LSP client
                            |
                       LSP over stdio
                            |
                  +-------------------+
                  | multilsp CLI      |
                  | transport + config|
                  +---------+---------+
                            |
                  +---------v---------+
                  | multilsp library  |
                  | protocol gateway  |
                  | capability manager|
                  | request router    |
                  | workspace manager |
                  | backend supervisor|
                  +---------+---------+
                            |
               +------------+------------+
               |            |            |
             gopls        clangd     rust-analyzer  ...
               |            |            |
          subprocesses communicating via LSP

       Build-time bundle -> embedded archive
                                 |
                          installer/verifier
                                 |
                       versioned app directory
```

### Core modules (proposed)

- **Gateway:** client JSON-RPC/LSP session and transport adaptation.
- **Router:** document-to-backend resolution and request fan-out/aggregation.
- **Capability manager:** negotiate and reconcile supported features.
- **Backend supervisor:** process creation, health, restart policy, shutdown.
- **Workspace manager:** roots, open documents, document versions, and language IDs.
- **Bundle manager:** embedded manifest, integrity checks, extraction, and locking.
- **Configuration:** language mappings, backend overrides, process options, and directories.

The exact crate split is not mandated. A simple initial layout is one Cargo package named `multilsp` containing `src/lib.rs` and `src/main.rs`, with internal modules and a packaging directory. Additional internal crates may be introduced only when justified.

## 7. Runtime bundle installation

### Default application paths

- Linux: `${XDG_DATA_HOME:-$HOME/.local/share}/multilsp/`
- macOS: `~/Library/Application Support/multilsp/`

Suggested layout:

```text
multilsp/
  versions/
    <bundle-id>/
      manifest.json
      bin/
        gopls
        clangd
        rust-analyzer
      runtimes/
        node/
        ...
  cache/
  locks/
```

`<bundle-id>` should identify the exact embedded bundle (for example, a content hash), rather than relying solely on the `multilsp` application version.

### Installation algorithm

1. Read the embedded bundle manifest and determine the expected bundle ID.
2. Check whether a completed installation with that ID exists and passes the configured validation policy.
3. If valid, reuse it without extraction.
4. Otherwise acquire a cross-process lock and recheck installation state.
5. Extract into a unique staging directory on the same filesystem as the final destination.
6. Validate extracted file sizes/hashes, paths, permissions, and required resources.
7. Atomically rename/publish the staging directory and record completion.
8. Release the lock and start the required backend processes.
9. On failure, report a precise error and leave previously valid installations untouched.

Archive extraction must reject path traversal, unsafe symlinks, and unexpected absolute paths. Installation must not trust writable cache contents solely because a completion marker exists. Define a lightweight startup check plus explicit full verification/repair path to balance speed and integrity.

## 8. CLI and public API (provisional)

```bash
multilsp serve --stdio
multilsp --version
multilsp backends list
multilsp bundle verify
multilsp bundle repair
```

Only `serve --stdio` is part of the initial intended interface; other commands are proposed operational conveniences.

Illustrative Rust API (not yet finalized):

```rust
use multilsp::{Config, LspManager};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let manager = LspManager::new(Config::default()).await?;
    manager.start().await?;
    // Host application controls the manager here.
    manager.shutdown().await?;
    Ok(())
}
```

Library consumers should be able to choose embedded or externally supplied backends. The default feature set for the published crate must be documented to avoid unexpectedly increasing dependent binary size.

## 9. Non-functional requirements

- **Portability:** run on supported Linux/macOS targets with documented minimum system requirements.
- **Reliability:** a crashed backend must not crash the entire gateway; failure and restart behavior must be defined.
- **Performance:** avoid extraction on healthy repeat launches; minimize routing overhead and unnecessary backend startup.
- **Security:** verify downloaded build artifacts; validate archive paths; avoid shell-based command execution for backend launch; treat backend processes as local executables with normal user privileges.
- **Observability:** structured logs to stderr or files, with backend identity and request correlation where practical.
- **Compatibility:** test real editor clients and supported LSP protocol behaviors.
- **Reproducibility:** record exact backend versions and build provenance.
- **Licensing:** review each backend/runtime license and provide required redistribution notices.

## 10. Testing and acceptance criteria

| ID | Acceptance criterion |
|---|---|
| AC-001 | `cargo build --release` on a supported target yields a `multilsp` executable containing the required bundle. |
| AC-002 | A clean target machine can run the executable without installing separate LSP servers or downloading packages. |
| AC-003 | First launch installs the embedded bundle; subsequent launches reuse it. |
| AC-004 | Deleting or corrupting an installed backend causes a verified repair rather than silent execution of invalid files. |
| AC-005 | Concurrent first launches cannot publish partial installations. |
| AC-006 | A client can initialize, open a Go document, and receive responses from `gopls` through `multilsp`. |
| AC-007 | Equivalent routing integration tests pass for C/C++, TypeScript/JavaScript, Python, and Rust. |
| AC-008 | A backend crash is reported and does not corrupt the client JSON-RPC stream. |
| AC-009 | A Rust host application can link the library and control backend lifecycle without invoking the `multilsp` CLI. |
| AC-010 | CI produces and smoke-tests artifacts for Linux/macOS x86_64/ARM64. |
| AC-011 | Core-only builds do not embed the full backend bundle. |
| AC-012 | All bundled components have recorded versions, checksums, sources, and required license notices. |

## 11. Suggested implementation phases

1. **Core protocol and process orchestration:** stdio gateway, backend supervisor, single-language end-to-end proof of concept with `gopls`.
2. **Multi-language routing:** workspace/document state, capability negotiation, remaining initial backends, diagnostics and error handling.
3. **Packaging pipeline:** pinned manifest, downloads/builds, runtime dependencies, cross-target resolution, embedded archive.
4. **Self-extraction:** versioned app data, locks, atomic installation, verification, repair, offline tests.
5. **Public library and CLI:** stable API boundary, configuration, optional embedded feature, operational commands.
6. **Portability and release hardening:** CI matrix, minimum OS requirements, licensing, performance, security, and editor compatibility tests.

## 12. Open decisions

1. **Backend selection:** Pyright vs alternatives; exact TypeScript/JavaScript implementation and runtime bundling.
2. **Linux portability:** minimum glibc baseline, possible musl variants, and how to package native `clangd` dependencies.
3. **Build reproducibility:** whether release builds must work fully offline from a populated cache; vendoring policy.
4. **Routing semantics:** behavior for requests involving multiple languages, multiple backends, or no document URI.
5. **Capability negotiation:** initial and dynamic registrations, backend-specific settings, and unsupported feature handling.
6. **Backend startup policy:** eager vs lazy, reuse across workspaces, restart limits, and idle shutdown.
7. **Library ergonomics:** exact async API, feature flags, transport abstractions, and ownership of runtime installation.
8. **Configuration:** format, locations, per-project overrides, and user-provided backend paths.
9. **Bundle lifecycle:** cleanup of obsolete versions, disk usage limits, and verification frequency.
10. **Release policy:** binary signing/notarization on macOS, provenance, and software bill of materials.

## 13. Explicitly deferred

- Windows support.
- Network-based LSP transport and remote backend execution.
- Custom language intelligence implemented directly by `multilsp`.
- Automatically downloading or updating language servers at application runtime.
- Universal macOS binaries (as opposed to separate x86_64 and ARM64 executables).

---

**Document note:** This specification captures agreed requirements and labels additional architectural suggestions as provisional. Exact API signatures, backend versions, and packaging recipes must be finalized during implementation design.
