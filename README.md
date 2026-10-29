# Cairn

Lightweight cross-platform browser (Linux, Windows, Android): Slint UI, Rust backend, Servo engine.

- **Containers**: tabs are grouped into containers; all tabs of a container share one engine instance
  (one process), so 17 tabs cost one engine, not 17. Containers are isolated from each other.
- **Ad blocking**: Brave's `adblock` crate with uBlock Origin's default lists (`cairn-core/src/lists.rs`).
- **Trade-off**: sharing a process saves RAM but weakens isolation inside a container. Use a mainstream
  browser for banking and other high-stakes sites.

Slint is pinned to 1.17 because Servo 0.7 and Slint 1.18 require incompatible ICU versions.

Status: UI + container model + blocker done; engine is a stub until built with `--features servo`.
