## What & why

Brief description of the change and the problem it solves. Link any related
issue (`Closes #123`).

## How

Key implementation notes — anything a reviewer needs to follow the diff.

## Checklist

- [ ] `just ci` passes (fmt check + `just test` + `cargo clippy` + wasm build + `just test-bash` + `just test-js`).
- [ ] Tests added/updated at the right layer (snapshot / `rail-reference.md` for render changes, unit/proptest for wire/parse).
- [ ] Snapshots reviewed with `just review` if render output changed.
- [ ] Docs updated (`README.md` / `docs/` / `CONTEXT.md`) if behavior or interfaces changed.
- [ ] Preserves the **push-driven** (no host polling) and **rail lockstep** invariants.
