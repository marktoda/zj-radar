# Security policy

Please report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/marktoda/zj-radar/security/advisories/new)
rather than a public issue. You should hear back within a few days.

The main supply-chain surface is distribution: the `curl | sh` installer and its
per-artifact `.sha256` checksum sidecars, and the CLI's checksum verification of
the downloaded sidebar wasm (`zj-radar setup zellij --download`). Reports about
weaknesses in that path are especially welcome. There is no bug bounty.

## Pipe trust model

The `zj_radar.status.v1` pipe has a local-session trust boundary: any process
inside the Zellij session (or another plugin, via `MessagePlugin`) can forge
payloads. The plugin treats them as untrusted display data — payloads over
64 KB are dropped whole, every text field is sanitized and truncated at parse
time, and notification commands receive the text as argv, never spliced into a
shell. What that cannot prevent: a local writer can always paint misleading
status. That is inherent to the boundary, not a vulnerability.
