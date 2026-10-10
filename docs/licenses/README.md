# Licence texts for crates that ship none

`tools/third-party-notices.mjs` embeds each shipped crate's own licence files (taken from the crate
archive). These six crates' archives contain no licence file, so the texts are kept here and
used as a fallback. Nothing in this directory is edited; each file is a byte-for-byte copy.

| Crates | Files here | Source |
|---|---|---|
| webview2-com, webview2-com-sys, webview2-com-macros (MIT) | `webview2-rs/LICENSE` | https://github.com/wravery/webview2-rs, `LICENSE` on `main` at edc2caf (fetched 2026-10-10) |
| defmt-parser (MIT OR Apache-2.0) | `defmt/LICENSE-MIT`, `defmt/LICENSE-APACHE` | https://github.com/knurling-rs/defmt, same names on `main` at a395e24 (fetched 2026-10-10); the crate's README points at these files |
| alloc-stdlib (BSD-3-Clause) | `rust-alloc-no-stdlib/LICENSE` | https://github.com/dropbox/rust-alloc-no-stdlib, `LICENSE` on `master` at 0a81fd6 (fetched 2026-10-10) |
| selectors (MPL-2.0) | `stylo-selectors/LICENSE-MPL-2.0` | https://github.com/servo/stylo has no licence file at any level (checked `main` at 64416a2, 2026-10-10). The Cargo.toml declares `MPL-2.0`, so this is the unmodified MPL-2.0 text copied from the `cssparser` 0.37.0 crate archive (also MPL-2.0, Servo project). |

The selectors entry carries no copyright line of its own: its Cargo.toml lists "The Servo Project
Developers" as author and that is all the source says.
