#!/usr/bin/env python3
"""Writes the ghz request arrays: ghz-data.py OUT_DIR START END. Seeded, so every server gets the
same requests in the same order."""
import json, random, sys
from pathlib import Path

out, start, end = Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
N = 5000
TXIDS = ["4cc852b50a27de5b297732199d4f02b5c7f5aa0e6337554f2fd2ea4e7c90029f",
         "28f0dfa4310e52bbd1a129457e9714805ffc43f03d9a1c0183f63e6136e78f4b"]
BUSY = "t1Jg2S2sJwDd223cHphY9cwTbznjeeLWaan"


def heights(seed, hi):
    r = random.Random(seed)
    return [r.randint(start, hi) for _ in range(N)]


def b64_wire(txid):
    import base64
    return base64.b64encode(bytes.fromhex(txid)[::-1]).decode()


data = {
    "GetLatestBlock": [{}],
    "GetLightdInfo": [{}],
    "GetLatestTreeState": [{}],
    "GetBlock": [{"height": h} for h in heights(1, end)],
    "GetBlockNullifiers": [{"height": h} for h in heights(2, end)],
    "GetTreeState": [{"height": h} for h in heights(3, end)],
    "GetTransaction": [{"hash": b64_wire(t)} for t in TXIDS],
    "GetTaddressBalance": [{"addresses": [BUSY]}],
    "GetSubtreeRoots": [{"shieldedProtocol": p, "startIndex": 0, "maxEntries": 10} for p in ("sapling", "orchard")],
    "GetBlockRange": [{"start": {"height": h}, "end": {"height": h + 99}} for h in heights(4, end - 99)],
    "GetBlockRangeNullifiers": [{"start": {"height": h}, "end": {"height": h + 999}} for h in heights(5, end - 999)],
}
for name, msgs in data.items():
    (out / f"{name}.json").write_text(json.dumps(msgs))
