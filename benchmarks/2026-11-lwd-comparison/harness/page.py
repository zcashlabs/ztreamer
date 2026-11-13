#!/usr/bin/env python3
"""Writes the site page and the Markdown report of the 2026-11 benchmark from numbers.json.

    page.py --data site/benchmarks/2026-11-13 --html site/benchmarks.html --md REPORT.md --md-images DIR

Both documents come from one list of sections, so they hold the same numbers and plots. The site
page shows the SVGs; the Markdown report embeds the PNGs, copied to --md-images.
"""
import argparse, html, json, math, shutil
from pathlib import Path

ORDER = ["ztreamer-direct", "ztreamer-remote", "lightwalletd"]
NAME = {"ztreamer-direct": "ztreamer direct", "ztreamer-remote": "ztreamer remote", "lightwalletd": "lightwalletd"}


def ok(v):
    return v is not None and not (isinstance(v, float) and not math.isfinite(v))


def ms(v):
    if not ok(v):
        return "–"
    if v < 1:
        return f"{v * 1000:.0f} µs"
    if v < 100:
        return f"{v:.2f} ms"
    if v < 1000:
        return f"{v:.0f} ms"
    return f"{v / 1000:.2f} s"


def secs(v):
    if not ok(v):
        return "–"
    if v < 120:
        return f"{v:.1f} s"
    if v < 7200:
        return f"{v / 60:.1f} min"
    return f"{v / 3600:.2f} h"


def gib(v):
    return f"{v / 2**30:.2f} GiB" if ok(v) else "–"


def num(v, d=0):
    return f"{v:,.{d}f}" if ok(v) else "–"


def ratio(a, b):
    """How many times b is larger than a, as text."""
    if not (ok(a) and ok(b)) or a <= 0:
        return "–"
    r = b / a
    return f"{r:.1f}×" if r < 100 else f"{r:,.0f}×"


class Doc:
    """Sections of paragraphs, plots and tables, rendered as HTML or as Markdown."""

    def __init__(self):
        self.sections = []

    def section(self, ident, title):
        self.sections.append(dict(id=ident, title=title, items=[]))

    def p(self, text):
        self.sections[-1]["items"].append(("p", text))

    def note(self, text):
        self.sections[-1]["items"].append(("note", text))

    def img(self, name, alt):
        self.sections[-1]["items"].append(("img", (name, alt)))

    def table(self, head, rows, cls=""):
        self.sections[-1]["items"].append(("table", (head, rows, cls)))

    def bullets(self, items):
        self.sections[-1]["items"].append(("ul", items))


def md_inline(t):
    return t.replace("<strong>", "**").replace("</strong>", "**").replace("<code>", "`").replace("</code>", "`")


def to_md(doc, title, intro, cards, images_rel):
    out = ["---", "tags: [zcash, ztreamer, lightwalletd, benchmark]", f"created: {intro['date']}", "---", "", f"# {title}", "",
           md_inline(intro["sub"]), ""]
    out += ["| Headline | Result | Detail |", "| --- | --- | --- |"]
    out += [f"| {c[0]} | **{c[1]}** | {c[2]} |" for c in cards]
    out.append("")
    for s in doc.sections:
        out += [f"## {s['title']}", ""]
        for kind, v in s["items"]:
            if kind == "p":
                out += [md_inline(v), ""]
            elif kind == "note":
                out += ["> " + md_inline(v), ""]
            elif kind == "ul":
                out += [f"- {md_inline(x)}" for x in v] + [""]
            elif kind == "img":
                out += [f"![{v[1]}]({images_rel}/{v[0]}.png)", ""]
            elif kind == "table":
                head, rows, _ = v
                out += ["| " + " | ".join(head) + " |", "|" + "|".join([" --- "] * len(head)) + "|"]
                out += ["| " + " | ".join(md_inline(str(c)) for c in r) + " |" for r in rows] + [""]
    return "\n".join(out)


def to_html(doc, title, intro, cards, assets, site):
    index = (site / "index.html").read_text()
    header = index.split("<header>", 1)[1].split("</header>", 1)[0].replace(
        '<a href="benchmarks.html">', '<a href="benchmarks.html" aria-current="page">')
    footer = index.split("<footer>", 1)[1].split("</footer>", 1)[0]
    e = html.escape
    h = ['<!doctype html><html lang="en"><head>', '<meta charset="utf-8">',
         '<meta name="viewport" content="width=device-width, initial-scale=1">', f"<title>ztreamer benchmarks</title>",
         '<meta property="og:title" content="ztreamer benchmarks">', '<meta name="theme-color" content="#171415">',
         '<link rel="icon" href="brand/favicon.svg" type="image/svg+xml">', '<link rel="apple-touch-icon" href="brand/apple-touch-icon.png">',
         '<link rel="preconnect" href="https://fonts.googleapis.com">', '<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>',
         '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Sora:wght@700;800&family=Manrope:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap">',
         '<link rel="stylesheet" href="site.css?v=3">',
         f'<meta name="description" content="{e(intro["description"])}">',
         '<link rel="stylesheet" href="benchmarks.css?v=4"></head><body class="benchmarks-page">',
         f"<header>{header}</header>", '<main class="wrap benchmark-report">',
         f'<div class="eyebrow">// {e(intro["eyebrow"])}</div><h1>{e(title)}</h1><p class="sub">{intro["sub"]}</p>',
         '<nav class="report-nav" aria-label="Benchmark sections">' + "".join(f'<a href="#{s["id"]}">{e(s["title"])}</a>' for s in doc.sections) + "</nav>",
         '<div class="cards cards-6">' + "".join(f'<div class="card">{e(c[0])}<b>{e(c[1])}</b><span>{e(c[2])}</span></div>' for c in cards) + "</div>"]
    for s in doc.sections:
        h.append(f'<section id="{s["id"]}"><h2>{e(s["title"])}</h2>')
        for kind, v in s["items"]:
            if kind == "p":
                h.append(f"<p>{v}</p>")
            elif kind == "note":
                h.append(f'<p class="note">{v}</p>')
            elif kind == "ul":
                h.append('<ul class="bullets">' + "".join(f"<li>{x}</li>" for x in v) + "</ul>")
            elif kind == "img":
                h.append(f'<img loading="lazy" alt="{e(v[1])}" src="{assets}{v[0]}.svg?v={intro["version"]}">')
            elif kind == "table":
                head, rows, cls = v
                h.append(f'<div class="table-wrap"><table class="{cls}"><thead><tr>' + "".join(f"<th>{e(x)}</th>" for x in head) + "</tr></thead><tbody>"
                         + "".join("<tr>" + "".join(f"<td>{c}</td>" for c in r) + "</tr>" for r in rows) + "</tbody></table></div>")
        h.append("</section>")
    h.append(f'</main><footer>{footer}</footer><script src="benchmarks-2026-11.js?v=1"></script></body></html>')
    return "".join(h)


def build(n, meta, md=False):
    R = [r for r in ORDER if r in n["runs"]]
    D = {r: n["runs"][r] for r in R}
    doc = Doc()
    g = lambda r, *path: _get(D.get(r, {}), path)
    zd, zr, lw = "ztreamer-direct", "ztreamer-remote", "lightwalletd"

    # ---------- cards ----------
    cards = []
    cards.append(("Sync, direct vs lightwalletd", ratio(g(zd, "sync", "seconds"), g(lw, "sync", "seconds")) + " faster",
                  f"ztreamer direct {secs(g(zd, 'sync', 'seconds'))} · lightwalletd {secs(g(lw, 'sync', 'seconds'))}"))
    cards.append(("Sync, remote from snapshot", secs(g(zr, "sync", "seconds")),
                  f"import and tail to tip · {ratio(g(zr, 'sync', 'seconds'), g(lw, 'sync', 'seconds'))} faster than lightwalletd"))
    cards.append(("Wallet download", ratio(g(zd, "wallet_download", "seconds"), g(lw, "wallet_download", "seconds")) + " faster",
                  f"direct {secs(g(zd, 'wallet_download', 'seconds'))} · lightwalletd {secs(g(lw, 'wallet_download', 'seconds'))}"))
    cards.append(("10,000-block range, p50", ratio(g(zd, "ranges", "10000", "p50_ms"), g(lw, "ranges", "10000", "p50_ms")) + " lower",
                  f"direct {ms(g(zd, 'ranges', '10000', 'p50_ms'))} · lightwalletd {ms(g(lw, 'ranges', '10000', 'p50_ms'))}"))
    cards.append(("Throughput at 128 clients", ratio(g(lw, "concurrency", "concurrency-10k", "128", "blocks_per_s"), g(zd, "concurrency", "concurrency-10k", "128", "blocks_per_s")) + " higher",
                  f"direct {num(_m(g(zd, 'concurrency', 'concurrency-10k', '128', 'blocks_per_s')), 2)} M blocks/s · lightwalletd {num(_m(g(lw, 'concurrency', 'concurrency-10k', '128', 'blocks_per_s')), 2)} M"))
    cards.append(("Disk footprint", f"{gib(g(zd, 'index_bytes'))}",
                  f"ztreamer index · lightwalletd cache {gib(g(lw, 'index_bytes'))}"))

    # ---------- summary ----------
    doc.section("summary", "Summary")
    doc.p(meta["summary"])
    doc.bullets(meta["findings"])

    # ---------- setup ----------
    doc.section("setup", "Setup")
    env = D[R[0]]["env"]
    doc.table(["", "Value"], [
        ["CPU", meta["hw"]["cpu"]], ["Memory", meta["hw"]["memory"]], ["Disk", meta["hw"]["disk"]],
        ["OS", meta["hw"]["os"]], ["CPU governor", env.get("governor", "–")],
        ["ztreamer", f"{env.get('ztreamerd_version', '')} · commit <code>{env.get('ztreamer_commit', '')[:10]}</code> · <code>maxperf</code> profile"],
        ["zakura", f"{env.get('zakurad_version', '')} (standalone node), the same version embedded in ztreamerd"],
        ["lightwalletd", f"commit <code>{D.get(lw, D[R[0]])['env'].get('lightwalletd_commit', '')[:10]}</code> · Go build, default flags"],
        ["Chain", f"zakura mainnet state from the archive, tip {num(meta['archive_tip'])}, synced to the live tip before each run"],
        ["Serving interval", f"{num(int(env.get('start', 0)))} to {num(int(env.get('end', 0)))}, fixed for every server"],
        ["Clients", "native Rust/Tonic serving suite of this repository · ghz v0.120.0 · loopback, plaintext HTTP/2"],
    ], "kv")
    doc.p("Each server ran alone on the host, in this order: ztreamer direct, ztreamer remote, lightwalletd. Every run started "
          "from an empty index. The zakura state came from the same archive and was brought to the live tip before each run, "
          "so no server timed the catch-up of its node. All three servers then served the same fixed interval, with the same "
          "seeded request sequences.")
    doc.bullets([
        "<strong>ztreamer direct</strong>: <code>ztreamerd --zakura-config</code>. The zakura node runs in the ztreamerd process and the index reads its state directly.",
        "<strong>ztreamer remote</strong>: <code>ztreamerd --remote-rpc</code> against a standalone zakurad. It imports the sealed ranges of the default snapshot source, <code>https://snapshot.ztreamer.sh/mainnet</code>, has the node attest them, and then reads the tail over JSON-RPC.",
        "<strong>lightwalletd</strong>: against the same standalone zakurad over JSON-RPC, with <code>--no-tls-very-insecure</code> and default limits. It builds its compact-block cache from genesis.",
    ])
    doc.note(meta["caveats"])

    # ---------- sync ----------
    doc.section("sync", "Sync speed")
    doc.p("Time from the start of the server process until its index has the tip of the synced node and its gRPC listener accepts connections.")
    doc.img("sync-speed", "Sync speed")
    rows = []
    for r in R:
        s = D[r]["sync"]
        rows.append([NAME[r], secs(s.get("seconds")), num(s.get("height")), num(s.get("blocks_per_s")), num(s.get("cpu_s")),
                     f"{s['cpu_s'] / s['seconds']:.1f}" if ok(s.get("cpu_s")) and s.get("seconds") else "–",
                     gib(s.get("rss_peak")), gib(s.get("read")), gib(s.get("write"))])
    doc.table(["Server", "Time to tip", "Tip", "Blocks / s", "CPU s", "Mean cores", "Peak RSS", "Disk read", "Disk written"], rows)
    doc.p(meta["sync_text"])
    doc.img("sync-rate", "Sync rate by height")
    ms_rows = []
    for r in R:
        s = D[r]["sync"]
        for k, label in [("import_start_s", "import planned"), ("import_complete_s", "import complete"), ("historical_complete_s", "historical pass complete"), ("serving_s", "gRPC listener open")]:
            if ok(s.get(k)):
                ms_rows.append([NAME[r], label, secs(s[k]), num(s.get(k.replace("_s", "_height")))])
    if ms_rows:
        doc.table(["Server", "Milestone", "After", "Height"], ms_rows)

    # ---------- resources ----------
    doc.section("resources", "Resource use")
    doc.p("A sampler read <code>/proc</code> once a second for each process: CPU time, resident memory and disk bytes. "
          "For the remote and lightwalletd runs the totals include the standalone zakurad, which they need; in the direct run the node is inside ztreamerd.")
    doc.img("sync-resources", "Resources while syncing")
    doc.img("index-growth", "Index growth")
    doc.img("disk-footprint", "Disk footprint")
    doc.img("serving-resources", "Resources while serving")
    groups = ["idle", "latency", "ranges", "concurrency 10k", "concurrency 100", "wallet download", "ghz"]
    rows = []
    for r in R:
        sr = D[r].get("serving_resources", {})
        for gname in groups:
            t = sr.get(gname)
            if t:
                rows.append([NAME[r], gname, secs(t["seconds"]), num(t["cpu_s"], 1), f"{t['cpu_s'] / t['seconds']:.2f}" if t["seconds"] else "–", gib(t["rss_peak"])])
    doc.table(["Server", "Workload", "Duration", "CPU s", "Mean cores", "Peak RSS"], rows)
    doc.p("<strong>Serving efficiency</strong>: compact blocks served per CPU second of the server and its node, in the workloads that serve blocks.")
    rows = []
    for gname in ["ranges", "concurrency 10k", "concurrency 100", "wallet download"]:
        row = [gname]
        for r in R:
            e = D[r].get("efficiency", {}).get(gname, {})
            row += [num(e.get("blocks")), num(e.get("cpu_s"), 1), num(e.get("blocks_per_cpu_s"))]
        rows.append(row)
    doc.table(["Workload"] + [f"{NAME[r]} {p}" for r in R for p in ("blocks", "CPU s", "blocks / CPU s")], rows)
    doc.p(meta["resource_text"])
    doc.img("restart-memory", "Restart and memory")
    rows = []
    for r in R:
        mem = D[r].get("memory", {})
        for k in ["server/idle", "server/load", "zakurad/idle", "zakurad/load"]:
            if k in mem:
                who, when = k.split("/")
                rows.append([NAME[r], "server" if who == "server" else "zakurad node", when, gib(mem[k]["anon"]), gib(mem[k]["file"]), gib(mem[k]["rss"])])
    doc.table(["Server", "Process", "When", "Anonymous", "File-backed", "RSS"], rows)
    rows = [[NAME[r], secs(D[r].get("restart_seconds")), secs(D[r].get("restart_wallet_seconds")),
             num((D[r].get("restart_concurrency", {}).get("64") or {}).get("blocks_per_s"))] for r in R]
    doc.table(["Server", "Restart to serving", "Wallet download after restart", "Blocks / s at 64 clients after restart"], rows)
    doc.p(meta["restart_text"])

    # ---------- serving ----------
    doc.section("serving", "Serving overview")
    doc.img("overview", "Serving overview")
    doc.p(meta["serving_text"])

    doc.section("rpc-latencies", "RPC latencies")
    doc.p("500 sequential requests per case from one client, after one warmup. Completion time includes draining the stream and decoding. "
          "Address cases use a busy and a quiet transparent address over the whole interval; GetTransaction uses two fixed transactions.")
    doc.img("rpc-latencies", "RPC latencies")
    cases = list(D[R[0]]["latency"].keys())
    rows = []
    for c in cases:
        row = [c]
        for r in R:
            x = D[r]["latency"].get(c, {})
            row += [ms(x.get("p50_ms")) if x.get("status") == "ok" else f"<em>{x.get('status', '–')}</em>", ms(x.get("p99_ms")) if x.get("status") == "ok" else "–"]
        if zd in D and lw in D:
            row.append(ratio(D[zd]["latency"].get(c, {}).get("p50_ms"), D[lw]["latency"].get(c, {}).get("p50_ms")))
        rows.append(row)
    doc.table(["Case"] + [f"{NAME[r]} {p}" for r in R for p in ("p50", "p99")] + (["LWD ÷ direct, p50"] if zd in D and lw in D else []), rows)

    doc.section("ranges", "Range sizes")
    doc.p("GetBlockRange over 1,000 seeded random ranges per size (100 at 100,000 blocks), one client, sequential.")
    doc.img("ranges", "Range sizes")
    rows = []
    for s in ["10", "100", "1000", "10000", "100000"]:
        row = [f"{int(s):,} blocks"]
        for r in R:
            x = D[r]["ranges"].get(s, {})
            row += [ms(x.get("p50_ms")), ms(x.get("p99_ms")), num(x.get("blocks_per_s"))]
        rows.append(row)
    doc.table(["Range"] + [f"{NAME[r]} {p}" for r in R for p in ("p50", "p99", "blocks/s")], rows)

    doc.section("concurrency", "Concurrent serving")
    doc.p("Closed loop: each client keeps one GetBlockRange outstanding for 20 s per level, on its own connection. Two request sizes: "
          "10,000 blocks (a wallet catching up) and 100 blocks (a wallet that is nearly synced).")
    doc.img("concurrency", "Concurrency")
    for suite, label in [("concurrency-10k", "10,000 blocks per request"), ("concurrency-100", "100 blocks per request")]:
        rows = []
        for c in ["1", "4", "16", "64", "128", "256"]:
            row = [c]
            for r in R:
                x = D[r]["concurrency"][suite].get(c, {})
                row += [num(x.get("blocks_per_s")), ms(x.get("p50_ms")), ms(x.get("p99_ms")), num(x.get("errors"))]
            rows.append(row)
        doc.p(f"<strong>{label}</strong>")
        doc.table(["Clients"] + [f"{NAME[r]} {p}" for r in R for p in ("blocks/s", "p50", "p99", "errors")], rows)

    doc.section("wallet-download", "Wallet download")
    doc.p("One client downloads every compact block from Sapling activation to the end of the interval, 10,000 blocks per request. "
          "This is the network part of a new wallet's first sync; it excludes trial decryption, scanning and storage.")
    doc.img("wallet-download", "Wallet download")
    rows = [[NAME[r], secs(D[r]["wallet_download"].get("seconds")), num(D[r]["wallet_download"].get("blocks")), gib(D[r]["wallet_download"].get("bytes")),
             num((D[r]["wallet_download"].get("blocks") or 0) / D[r]["wallet_download"]["seconds"]) if ok(D[r]["wallet_download"].get("seconds")) else "–"] for r in R]
    doc.table(["Server", "Time", "Blocks", "Protobuf bytes", "Blocks / s"], rows)

    doc.section("per-rpc", "Throughput per RPC")
    doc.p("ghz drives one RPC at a time with 1, 16, 64 and 256 concurrent workers, one connection each, for 15 s per level. Requests with heights "
          "cycle through 5,000 seeded random heights. The first request of each worker is not counted. Throughput counts successful requests only.")
    doc.img("ghz-throughput", "Throughput per RPC")
    doc.img("ghz-latency", "Latency per RPC")
    rpcs = list(dict.fromkeys(k.split("/")[0] for k in D[R[0]]["ghz"]))
    rows = []
    for rpc in rpcs:
        for c in ["1", "16", "64", "256"]:
            row = [rpc, c]
            for r in R:
                x = D[r]["ghz"].get(f"{rpc}/{c}", {})
                row += [num(x.get("rps")), ms(x.get("p50_ms")), ms(x.get("p99_ms")), num(x.get("errors"))]
            rows.append(row)
    doc.table(["RPC", "Workers"] + [f"{NAME[r]} {p}" for r in R for p in ("req/s", "p50", "p99", "errors")], rows, "filterable")

    doc.section("data", "Raw data")
    doc.p(meta["data_links_md"] if md else meta["data_links"])
    return cards, doc


def _m(v):
    return v / 1e6 if ok(v) else None


def _get(d, path):
    for k in path:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--data", required=True)
    p.add_argument("--meta", required=True)
    p.add_argument("--html")
    p.add_argument("--md")
    p.add_argument("--md-images")
    a = p.parse_args()
    data = Path(a.data)
    n = json.loads((data / "numbers.json").read_text())
    meta = json.loads(Path(a.meta).read_text())
    title = "ztreamer vs lightwalletd"
    if a.html:
        cards, doc = build(n, meta)
        site = Path(a.html).parent
        assets = str(data.relative_to(site)) + "/"
        Path(a.html).write_text(to_html(doc, title, meta["intro"], cards, assets, site))
    if a.md:
        cards, doc = build(n, meta, md=True)
        imgs = Path(a.md_images)
        imgs.mkdir(parents=True, exist_ok=True)
        for f in data.glob("*.png"):
            shutil.copy(f, imgs / f.name)
        rel = imgs.relative_to(Path(a.md).parent)
        Path(a.md).write_text(to_md(doc, title, meta["intro"], cards, str(rel).replace(" ", "%20")))


if __name__ == "__main__":
    main()
