## What and why

<!-- What this changes, and why. Link the issue it fixes, if any. -->

## Checklist

- [ ] The gates pass locally (`cargo fmt`, clippy with `-D warnings`, the tests, `cargo doc`,
      `cargo run -p xtask -- quality`); see CONTRIBUTING.md.
- [ ] Tests cover the change, with the `fake` test doubles where a tool is involved.
- [ ] `CHANGELOG.md` has an entry under `[Unreleased]`, starting with **Breaking:** if it breaks
      something docs/policies.md calls stable (and the migration guide says what to do).
- [ ] The docs the change makes wrong are updated (`docs/`, crate READMEs, rustdoc).
- [ ] No secret, `.cargo/config.toml` or locally patched `Cargo.lock` is committed.

## Real runs

<!-- Which real runs you did (`cargo run -p xtask -- real-run`, a cloud, a hypervisor), or "none". -->
