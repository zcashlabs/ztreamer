#!/usr/bin/env python3
"""Prints the height that the note commitment trees of a ztreamerd index reach, or nothing.

ztreamer v0.2.0 logs no line when its tree builder catches up with the durable tip, so the harness
reads the `trees` row of the `metadata` table in a read-only LMDB transaction beside the running
ztreamerd. It uses the `trees_through` example of the ztreamer v0.2.0 tree (heed, the LMDB build of
ztreamerd): py-lmdb bundles another LMDB version, whose lock file format does not match.
Build: cargo build --release -p ztreamer-index --example trees_through. Usage: zt-trees.py INDEX_DIR
"""
import os, subprocess, sys

BIN = os.environ.get("TREES_THROUGH", os.path.expanduser(
    "~/dev/zcash/ztreamer-v0.2.0/target/release/examples/trees_through"))


def trees_through(index_dir):
    try:
        out = subprocess.run([BIN, index_dir], capture_output=True, text=True, timeout=30)
        return out.stdout.strip() if out.returncode == 0 else ""
    except (OSError, subprocess.TimeoutExpired):
        return ""


if __name__ == "__main__":
    print(trees_through(sys.argv[1]))
