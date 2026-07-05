# zj-radar-core

The shared vocabulary and wire crate for [zj-radar](https://github.com/marktoda/zj-radar),
a Zellij sidebar that shows per-tab AI-agent status.

It owns the versioned `zj_radar.status.v1` pipe payload (parse, sanitize,
`to_wire`), the `Status`/`Kind` vocabulary, and the pure observed-command
classification — no clap, no zellij-tile, serde only. Both halves of zj-radar
(the host CLI and the wasm plugin) depend on it, so the wire contract can
never drift between them.

Depend on this crate if you are writing a **producer**: a tool that broadcasts
`zj_radar.status.v1` payloads for the sidebar to display. Build a
`StatusPayload` (use `..Default::default()` for fields you don't set) and
serialize it with `to_wire`; see the crate docs for a worked example.

Everything else — the sidebar, the CLI, setup instructions — lives in the
[main repository](https://github.com/marktoda/zj-radar).
