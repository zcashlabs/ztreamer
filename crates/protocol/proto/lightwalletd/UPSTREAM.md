# lightwalletd Protocol Provenance

The vendored `compact_formats.proto` and `service.proto` files in this
directory are pinned to the upstream content below, with trailing whitespace
normalized for the repository hygiene gate:

- Repository: `https://github.com/zcash/lightwallet-protocol`
- Commit: `299805c19011a027d2c48c5e912dd05041f76156` (tag `v0.6.0`)
- Source paths:
  - `walletrpc/compact_formats.proto`
  - `walletrpc/service.proto`
- Verified on: 2026-11-13

Use the adjacent `COMMIT` file as the machine-readable source of this pin when
deciding whether a future upstream protocol change is an intentional Zinder
compatibility update or unreviewed drift.

Verification:

```bash
diff -u \
  <(perl -pe 's/[ \t]+$//' crates/protocol/proto/lightwalletd/compact_formats.proto) \
  <(curl -fsSL https://raw.githubusercontent.com/zcash/lightwallet-protocol/299805c19011a027d2c48c5e912dd05041f76156/walletrpc/compact_formats.proto | perl -pe 's/[ \t]+$//')

diff -u \
  <(perl -pe 's/[ \t]+$//' crates/protocol/proto/lightwalletd/service.proto) \
  <(curl -fsSL https://raw.githubusercontent.com/zcash/lightwallet-protocol/299805c19011a027d2c48c5e912dd05041f76156/walletrpc/service.proto | perl -pe 's/[ \t]+$//')
```
