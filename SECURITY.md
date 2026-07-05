# Security policy

Please report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/marktoda/zj-radar/security/advisories/new)
rather than a public issue. You should hear back within a few days.

The main supply-chain surface is distribution: the `curl | sh` installer and its
per-artifact `.sha256` checksum sidecars, and the CLI's checksum verification of
the downloaded sidebar wasm (`zj-radar setup zellij --download`). Reports about
weaknesses in that path are especially welcome. There is no bug bounty.
