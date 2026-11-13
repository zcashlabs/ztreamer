# ztreamer and lightwalletd, November 2026

This directory has the harness of the benchmark that the site shows at `/benchmarks`
(`site/benchmarks.html`). It compares three servers on the same machine, one after the other:

| Label | Server | Chain source |
| --- | --- | --- |
| `ztreamer-direct` | `ztreamerd --zakura-config` | zakura embedded in the ztreamerd process |
| `ztreamer-remote` | `ztreamerd --remote-rpc` | the default snapshot (`https://snapshot.ztreamer.sh/mainnet`), then a standalone zakurad over JSON-RPC |
| `lightwalletd` | `lightwalletd` | the same standalone zakurad over JSON-RPC |

The data and the plots are in `site/benchmarks/2026-11-13/`.

## What it measures

- **Sync**: process start to an index at the tip of the synced node, with the gRPC listener open.
  Every run starts with an empty index. Each node starts from the same archive of zakura state, and
  the harness brings it to the live tip before the server starts. The time of the server therefore
  never includes the catch-up of its node.
- **Resources**: a sampler (`sample.py`) reads `/proc` once a second for the server and its node:
  CPU time, RSS (and from the restart runs on, anonymous and file-backed RSS), threads, and disk
  bytes. Every 15 s it records the size of the index directory.
- **Serving** (`serve.sh`), on the fixed interval 419,200 to 3,564,614 (the tip of the archive):
  - the native serving suite of this repository (`crates/service/examples/serving-suite.rs`):
    per-RPC latency (500 requests per case), GetBlockRange at 10 to 100,000 blocks, closed-loop
    concurrency at 1 to 256 clients with 10,000- and 100-block requests, and the wallet download
    from Sapling activation to the end of the interval;
  - ghz v0.120.0 for 11 RPCs at 1, 16, 64 and 256 workers, 15 s each, with seeded request arrays
    (`ghz-data.py`).
- **Restart** (`run-restart.sh`): each server restarted on its finished index; time to serving,
  memory at idle and under load, and a second wallet download.

## Running it

Build first: `just maxperf` (ztreamerd), `cargo build --release -p ztreamer-service --example
serving-suite`, zakurad v1.8.4 with `--features indexer`, lightwalletd with `go build`, and
`go install github.com/bojand/ghz/cmd/ghz@v0.120.0`. `common.sh` names every path; override them
from the environment. Extract the zakura archive into `$ROOT/zakura-cache`. Then, one at a time:

```sh
bash benchmarks/2026-11-lwd-comparison/harness/run-direct.sh
bash benchmarks/2026-11-lwd-comparison/harness/run-remote.sh
bash benchmarks/2026-11-lwd-comparison/harness/run-lwd.sh
for m in direct remote lwd; do bash benchmarks/2026-11-lwd-comparison/harness/run-restart.sh $m; done
```

Each run writes `$ROOT/runs/<mode>`: `phases.csv` (start and end of each phase), `resources.csv`,
`resources-disk.csv`, the server and node logs, and `serving/`. `report.py` turns the run
directories into the CSVs, plots and `numbers.json`; `page.py` writes the site page and the Markdown
report from `numbers.json` and `meta.json`.

```sh
python report.py --run ztreamer-direct=RUNS/direct --run ztreamer-remote=RUNS/remote \
  --run lightwalletd=RUNS/lwd --restart ztreamer-direct=RUNS/direct-restart \
  --restart ztreamer-remote=RUNS/remote-restart --restart lightwalletd=RUNS/lwd-restart \
  --out site/benchmarks/2026-11-13
python page.py --data site/benchmarks/2026-11-13 --meta benchmarks/2026-11-lwd-comparison/meta.json \
  --html site/benchmarks.html --md REPORT.md --md-images IMAGES_DIR
```

`report.py` and `page.py` need matplotlib and numpy (`scripts/benchmark-report-requirements.txt`).

## Notes on the method

- The node follows the live chain, so tip-dependent RPCs (GetLatestBlock, GetLatestTreeState,
  GetMempoolTx) see slightly different tips in each run. Every historical request is in the fixed
  interval.
- The client and the servers share the host and talk over loopback without TLS.
- No caches were dropped between runs. The direct run read the zakura state right after the
  archive was extracted; later runs found more of it in the page cache.
- lightwalletd builds its cache from genesis; ztreamer indexes from genesis too.
- ztreamerd keeps non-finalized blocks out of its durable height, so the harness counts it synced
  within 100 blocks of the node tip; lightwalletd must reach the node tip.
- After its first sync, ztreamerd fills the range metadata of every sealed range (the Replicator).
  The harness waits until that pass has been quiet for 60 s before it serves, and reports its time.
