# Security policy

Please report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/marktoda/zj-radar/security/advisories/new)
rather than a public issue. You should hear back within a few days.

The main supply-chain surface is distribution: the `curl | sh` installer and its
per-artifact `.sha256` checksum sidecars, and the CLI's checksum verification of
the downloaded sidebar wasm — the same code path behind
`zj-radar setup zellij --download` *and* `zj-radar run`'s first-use wasm fetch
on a crates.io-installed CLI. Be aware the verification **fails open** by
design: a missing `.sha256` sidecar, or no local sha256 tool
(`sha256sum`/`shasum`), downgrades to a warning and installs anyway, with TLS +
GitHub release storage as the floor; only an actual digest mismatch aborts
(`crates/cli/src/setup/download.rs`, `scripts/install.sh`). Reports about
weaknesses in that path are especially welcome. There is no bug bounty.

Dependency advisories are monitored by a nightly
`cargo deny check advisories licenses sources` CI job (config in `deny.toml`);
a failure automatically files a tracking issue.

## Pipe trust model

The `zj_radar.status.v1` pipe has a local-session trust boundary: any process
inside the Zellij session (or another plugin, via `MessagePlugin`) can forge
payloads. The plugin treats them as untrusted display data — payloads over
64 KB are dropped whole, every text field is sanitized and truncated at parse
time, and notification commands receive the text as argv, never spliced into a
shell. What that cannot prevent: a local writer can always paint misleading
status. That is inherent to the boundary, not a vulnerability.
