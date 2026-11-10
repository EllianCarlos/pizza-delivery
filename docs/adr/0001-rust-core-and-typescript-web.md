# Rust core plus TypeScript web

Status: accepted

The system is written in Rust in a functional style: a pure, publishable core crate (`pizza-delivery-core`) holds state transitions of the form `(state, event) -> (state, effects)`, and a binary crate (`pizza-delivery`) holds the daemon, CLI and TUI. Side effects (SQLite, subprocesses, git, transport) sit behind traits at the edges. `unwrap`, `expect` and `panic` are denied through workspace clippy lints. The web UI is TypeScript on Bun with React and Cloudscape, living in `web/`.

## Why

A long-running daemon that supervises other processes benefits from Rust's type system, absence of a runtime and single static binary. A pure core makes the state machine testable without I/O. The browser needs JavaScript, and Cloudscape and React are the owner's chosen UI stack, so the web client stays TypeScript rather than compiling Rust to WASM.

## Consequences

- Request, response and view-model types are defined once in the core crate and mirrored to TypeScript; CI carries a type-drift check.
- Web assets are embedded in the binary; Bun is needed only for development and Nix and release builds.
- Hard to reverse: the core's types and the lint policy shape every later module.
