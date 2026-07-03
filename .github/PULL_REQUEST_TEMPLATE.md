## What & why

Brief description of the change and the problem it solves. Link any related
issue (`Closes #123`).

## How

Key implementation notes — anything a reviewer needs to follow the diff.

## Checklist

- [ ] `just ci` passes (`just test` + `cargo clippy` + `just test-bash`).
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- [ ] Did **not** run `cargo fmt` (this repo is hand-formatted — see CONTRIBUTING.md).
- [ ] Tests added/updated at the right layer (snapshot / `rail-reference.md` for render changes, unit/proptest for wire/parse).
- [ ] Snapshots reviewed with `just review` if render output changed.
- [ ] Docs updated (`README.md` / `docs/` / `CONTEXT.md`) if behavior or interfaces changed.
- [ ] Preserves the **push-driven** (no host polling) and **rail lockstep** invariants.
