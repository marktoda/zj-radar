---
name: Bug report
about: Something in the sidebar, CLI, or producer isn't behaving
title: ""
labels: bug
assignees: ""
---

## What happened

A clear description of the bug.

## Expected

What you expected instead.

## Repro

Steps to reproduce. A minimal layout snippet or `zellij pipe` command that
triggers it is ideal:

```sh
# e.g. the broadcast you sent, or the layout you loaded
```

## Environment

- zj-radar version / commit:
- Zellij version (`zellij --version`):
- OS:
- Producer: Claude Code plugin / Codex / Opencode / pi / `notify generic` / custom (`zj_radar.status.v1`)
- Installed via: `install.sh` / `cargo install` / Nix / release artifact / build from source
- `zj-radar setup --check` output:

## Notes

Anything else — screenshots of the rail, relevant config (`density`/`naming`/
`glyphs`), whether it's reproducible or intermittent.
