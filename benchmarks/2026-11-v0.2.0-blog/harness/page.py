#!/usr/bin/env python3
"""Writes the site page and the Markdown report of the ztreamer v0.2.0 benchmark from numbers.json.

    page.py --data site/benchmarks/2026-11-18 --meta meta.json --html site/benchmarks.html --md REPORT.md --md-images DIR

Both documents come from one list of sections, so they hold the same numbers and plots. The site
page shows the SVGs; the Markdown report embeds the PNGs, copied to --md-images.

The site page compares ztreamer with lightwalletd by default and has toggles for lightwalletd and
zingod. Each part of the page that is about one of them carries data-srv="lw" or "zg" (both: hidden
when either is off; "any": hidden only when both are off), and each plot switches to the SVG of
report.py's v/<key>/ for the servers that are on. The Markdown report shows every server.
A text in meta.json is a string or a list of [text, srv] segments.
"""
import argparse, html, json, math, shutil
from pathlib import Path

ORDER = ["ztreamer-direct", "ztreamer-snapshot", "lightwalletd", "zingod-direct", "zingod-snapshot"]
NAME = {"ztreamer-direct": "ztreamer direct", "ztreamer-snapshot": "ztreamer snapshot", "lightwalletd": "lightwalletd",
        "zingod-direct": "zingod direct", "zingod-snapshot": "zingod snapshot"}
WALLET = ["ztreamer-direct", "lightwalletd", "zingod-direct"]
TOGGLES = [("lw", "lightwalletd", True), ("zg", "zingod", False)]


def grp(r):
    """The toggle group of a server label: '' for ztreamer, which is always shown."""
    return "lw" if r == "lightwalletd" else ("zg" if r.startswith("zingod") else "")


def seg_html(t):
    if isinstance(t, str):
        return t
    return "".join(f'<span data-srv="{g}">{x}</span>' if g else x for x, g in t)


def seg_md(t):
    return md_inline(t if isinstance(t, str) else "".join(x for x, _ in t))


def seg_srv(t):
    """The group of a text whose segments all share one group, else ''."""
    gs = {g for _, g in t} if not isinstance(t, str) else {""}
    return gs.pop() if len(gs) == 1 else ""


def srv_attr(g):
    return f' data-srv="{g}"' if g else ""


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

    def table(self, head, rows, cls="", cols=None, rows_srv=None):
        """cols: the toggle group of each column; rows_srv: the toggle group of each row."""
        self.sections[-1]["items"].append(("table", (head, rows, cls, cols or [""] * len(head), rows_srv or [""] * len(rows))))

    def bullets(self, items):
        self.sections[-1]["items"].append(("ul", items))


def md_inline(t):
    return t.replace("<strong>", "**").replace("</strong>", "**").replace("<code>", "`").replace("</code>", "`")


def to_md(doc, title, intro, cards, images_rel):
    out = ["---", "tags: [zcash, ztreamer, lightwalletd, zingod, benchmark]", f"created: {intro['date']}", "---", "", f"# {seg_md(title)}", "",
           seg_md(intro["sub"]), ""]
    out += ["| Headline | Result | Detail |", "| --- | --- | --- |"]
    out += [f"| {c[0]} | **{c[1]}** | {seg_md(c[2])} |" for c in cards]
    out.append("")
    for s in doc.sections:
        out += [f"## {s['title']}", ""]
        for kind, v in s["items"]:
            if kind == "p":
                out += [seg_md(v), ""]
            elif kind == "note":
                out += ["> " + seg_md(v), ""]
            elif kind == "ul":
                out += [f"- {seg_md(x)}" for x in v] + [""]
            elif kind == "img":
                out += [f"![{v[1]}]({images_rel}/{v[0]}.png)", ""]
            elif kind == "table":
                head, rows = v[0], v[1]
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
         '<link rel="stylesheet" href="benchmarks.css?v=5"></head>',
         '<body class="benchmarks-page' + "".join(f" hide-{g}" for g, _, on in TOGGLES if not on) + '">',
         f"<header>{header}</header>", '<main class="wrap benchmark-report">',
         f'<div class="eyebrow">// {e(intro["eyebrow"])}</div><h1>{seg_html(title)}</h1><p class="sub">{seg_html(intro["sub"])}</p>',
         '<div class="server-toggles" role="group" aria-label="Servers compared with ztreamer"><span class="lead">Compare ztreamer with</span>'
         + "".join(f'<label class="compare-toggle"><input type="checkbox" data-srv-toggle="{g}"{" checked" if on else ""}> {e(name)}</label>'
                   for g, name, on in TOGGLES) + "</div>",
         '<nav class="report-nav" aria-label="Benchmark sections">' + "".join(f'<a href="#{s["id"]}">{e(s["title"])}</a>' for s in doc.sections) + "</nav>",
         '<div class="cards cards-6">' + "".join(f'<div class="card">{e(c[0])}<b>{e(c[1])}</b><span>{seg_html(c[2])}</span></div>' for c in cards) + "</div>"]
    default = "-".join(g for g, _, on in TOGGLES if on) or "none"
    for s in doc.sections:
        h.append(f'<section id="{s["id"]}"><h2>{e(s["title"])}</h2>')
        for kind, v in s["items"]:
            if kind == "p":
                h.append(f"<p>{seg_html(v)}</p>")
            elif kind == "note":
                h.append(f'<p class="note">{seg_html(v)}</p>')
            elif kind == "ul":
                h.append('<ul class="bullets">' + "".join(f"<li{srv_attr(seg_srv(x))}>{seg_html(x)}</li>" for x in v) + "</ul>")
            elif kind == "img":
                h.append(f'<img loading="lazy" alt="{e(v[1])}" data-plot="{v[0]}" data-base="{assets}" data-ver="{intro["version"]}" '
                         f'src="{assets}v/{default}/{v[0]}.svg?v={intro["version"]}">')
            elif kind == "table":
                head, rows, cls, cols, rows_srv = v
                h.append(f'<div class="table-wrap"><table class="{cls}"><thead><tr>' + "".join(f"<th{srv_attr(g)}>{e(x)}</th>" for x, g in zip(head, cols))
                         + "</tr></thead><tbody>"
                         + "".join(f"<tr{srv_attr(rg)}>" + "".join(f"<td{srv_attr(g)}>{c}</td>" for c, g in zip(r, cols)) + "</tr>"
                                   for r, rg in zip(rows, rows_srv)) + "</tbody></table></div>")
        h.append("</section>")
    h.append(f'</main><footer>{footer}</footer><script src="benchmarks-2026-11.js?v=2"></script></body></html>')
    return "".join(h)


def tsec(v):
    """A wall time in seconds, with ms below one second."""
    if not ok(v):
        return "–"
    return ms(v * 1000) if v < 1 else (f"{v:.2f} s" if v < 120 else secs(v))


def cores(cpu_s, seconds):
    return f"{cpu_s / seconds:.1f}" if ok(cpu_s) and ok(seconds) and seconds else "–"


def served_at(D, r):
    """Seconds from process start to an index at the node tip with the gRPC listener open."""
    return _get(D[r], ("sync", "seconds"))


def trees_at(D, r):
    """Seconds from process start to note commitment trees at the tip (ztreamer only)."""
    t = _get(D[r], ("phases", "trees", "seconds"))
    s = served_at(D, r)
    return s + t if ok(t) and ok(s) else None


def conc_cell(x):
    st = x.get("status")
    if st and st != "ok":
        errs = x.get("errors")
        return f"<em>{st}</em>" + (f" ({num(errs)} errors)" if ok(errs) and errs else "")
    return None


def build(n, meta, md=False):
    R = [r for r in ORDER if r in n["runs"]]
    D = {r: n["runs"][r] for r in R}
    W = [r for r in WALLET if r in D and D[r].get("cbb")]
    doc = Doc()
    g = lambda r, *path: _get(D.get(r, {}), path)
    zd, zs, lw, gd, gs = ORDER
    cols = lambda runs, per=1, lead=1: [""] * lead + [grp(r) for r in runs for _ in range(per)]
    rsrv = lambda runs: [grp(r) for r in runs]

    # ---------- cards ----------
    vs = lambda zt, l, z: [(zt, ""), (f" · lightwalletd {l}", "lw"), (f" · zingod {z}", "zg")]
    cards = [
        ("Empty index to serving", secs(served_at(D, zd)),
         vs(f"ztreamer direct · trees ready at {secs(trees_at(D, zd))}", secs(served_at(D, lw)), f"direct {secs(served_at(D, gd))}")),
        ("From a snapshot", secs(served_at(D, zs)),
         [("ztreamer snapshot · default source", ""), (f" · zingod snapshot {secs(served_at(D, gs))}", "zg")]),
        ("Wallet download", secs(g(zd, "wallet_download", "seconds")),
         vs("ztreamer direct", secs(g(lw, "wallet_download", "seconds")), f"direct {secs(g(gd, 'wallet_download', 'seconds'))}")),
        ("10,000-block range, p50", ms(g(zd, "ranges", "10000", "p50_ms")),
         vs("ztreamer direct", ms(g(lw, "ranges", "10000", "p50_ms")), f"direct {ms(g(gd, 'ranges', '10000', 'p50_ms'))}")),
        ("GetTreeState, p50", ms(g(zd, "latency", "GetTreeState", "p50_ms")),
         vs("ztreamer direct", ms(g(lw, "latency", "GetTreeState", "p50_ms")), f"direct {ms(g(gd, 'latency', 'GetTreeState', 'p50_ms'))}")),
        ("Disk footprint", gib(g(zd, "index_bytes")),
         vs("ztreamer index", gib(g(lw, "index_bytes")), gib(g(gd, "index_bytes")))),
    ]

    # ---------- summary ----------
    doc.section("summary", "Summary")
    doc.p(meta["summary"])
    rows = []
    for r in R:
        s = D[r]["sync"]
        rows.append([NAME[r], secs(served_at(D, r)), secs(trees_at(D, r)) if r.startswith("ztreamer") else "–",
                     secs(g(r, "wallet_download", "seconds")), ms(g(r, "ranges", "10000", "p50_ms")), ms(g(r, "ranges", "10", "p50_ms")),
                     ms(g(r, "latency", "GetTreeState", "p50_ms")),
                     num(_m(g(r, "concurrency", "concurrency-10k", "64", "blocks_per_s")), 2),
                     num(s.get("cpu_s")), gib(s.get("rss_peak")), gib(g(r, "index_bytes")), secs(g(r, "restart_seconds"))])
    doc.table(["Server", "Empty index to serving", "Trees ready", "Wallet download", "10,000-block range p50", "10-block range p50",
               "GetTreeState p50", "M blocks/s at 64 clients", "Sync CPU s", "Sync peak RSS", "Disk", "Restart to serving"], rows, rows_srv=rsrv(R))
    doc.note([("Sync CPU and peak RSS add the standalone zakurad for every server but ztreamer direct, which runs zakura in its own process. "
               "Trees ready is when the note commitment trees of ztreamer reach the tip", ""),
              ("; the other servers have no such step", "any"), (" (", "any"), ("lightwalletd asks the node for each tree state", "lw"),
              (", ", "lw zg"), ("zingod builds its tree-state index during its sync", "zg"), (")", "any"), (".", "")])
    doc.bullets(meta["findings"])

    # ---------- setup ----------
    doc.section("setup", "Setup")
    env = D[R[0]]["env"]
    e_lw = D.get(lw, D[R[0]])["env"]
    e_gd = D.get(gd, D[R[0]])["env"]
    srows = [
        ["CPU", meta["hw"]["cpu"]], ["Memory", meta["hw"]["memory"]], ["Disk", meta["hw"]["disk"]],
        ["OS", meta["hw"]["os"]], ["CPU governor", env.get("governor", "–")],
        ["ztreamer", f"{env.get('ztreamerd_version', '')} · commit <code>{env.get('ztreamerd_commit', '')[:10]}</code> · <code>maxperf</code> profile"],
        ["zakura", f"{env.get('zakurad_version', '')} · commit <code>{env.get('zakurad_commit', '')[:10]}</code> (standalone node), the version embedded in ztreamerd 0.2.0"],
        ["lightwalletd", f"{e_lw.get('lightwalletd_tag', '')} · commit <code>{e_lw.get('lightwalletd_commit', '')[:10]}</code> · Go build, default flags"],
        ["zingod", f"{e_gd.get('zingod_version', '')} · commit <code>{e_gd.get('zingod_commit', '')[:10]}</code> · release build, <code>generate-config</code> defaults"],
        ["Wallet benchmarks", meta["wallet_tools"]],
        ["Chain", meta["chain"]],
        ["Serving interval", f"{num(int(env.get('start', 0)))} to {num(int(env.get('end', 0)))}, fixed for every server"],
        ["Clients", "native Rust/Tonic serving suite of this repository · ghz · zcash-walletsync-bench · cbb · loopback, plaintext HTTP/2"],
    ]
    doc.table(["", "Value"], srows, "kv", rows_srv=[{"lightwalletd": "lw", "zingod": "zg"}.get(x[0], "") for x in srows])
    doc.p([("Each server ran alone on the host, in this order: ztreamer direct, ztreamer snapshot", ""), (", lightwalletd", "lw"),
           (", zingod direct, zingod snapshot", "zg"),
           (". Every run started from an empty index or data directory. The standalone zakurad started on the same state and was brought to the "
            "live tip before each server started, so no server timed the catch-up of its node. All servers then served the same fixed interval "
            "with the same seeded request sequences.", "")])
    doc.bullets(meta["labels"])
    doc.note(meta["caveats"])

    # ---------- sync ----------
    doc.section("sync", "Sync speed")
    doc.p([("Time from the start of the server process until its index has the tip of the synced node and its gRPC listener accepts connections.", ""),
           (" For zingod, synced means <code>/status</code> reports the served tip within one block of the node.", "zg")])
    doc.img("sync-speed", "Sync speed")
    rows = []
    for r in R:
        s = D[r]["sync"]
        rows.append([NAME[r], secs(s.get("seconds")), num(s.get("height")), num(s.get("blocks_per_s")), num(s.get("cpu_s")),
                     cores(s.get("cpu_s"), s.get("seconds")), gib(s.get("rss_peak")), gib(s.get("read")), gib(s.get("write"))])
    doc.table(["Server", "Time to tip", "Tip", "Blocks / s", "CPU s", "Mean cores", "Peak RSS", "Disk read", "Disk written"], rows, rows_srv=rsrv(R))
    doc.p(meta["sync_text"])
    doc.img("sync-rate", "Sync rate by height")
    ms_rows, ms_srv = [], []
    for r in R:
        s = D[r]["sync"]
        for k, label in [("import_start_s", "import planned"), ("import_complete_s", "import complete"),
                         ("historical_complete_s", "historical pass complete"), ("serving_s", "gRPC listener open")]:
            if ok(s.get(k)):
                ms_rows.append([NAME[r], label, secs(s[k]), num(s.get(k.replace("_s", "_height")))])
                ms_srv.append(grp(r))
        if r.startswith("ztreamer") and ok(trees_at(D, r)):
            ms_rows.append([NAME[r], "note commitment trees at the tip", secs(trees_at(D, r)), "–"])
            ms_srv.append("")
    if ms_rows:
        doc.table(["Server", "Milestone", "After", "Height"], ms_rows, rows_srv=ms_srv)

    # ---------- snapshot vs direct ----------
    doc.section("snapshot", "Snapshot against direct")
    doc.p(meta["snapshot_text"])
    rows, srv = [], []
    for a, b in [(zd, zs), (gd, gs)]:
        for r in (a, b):
            srv.append(grp(r))
            s = D[r]["sync"]
            rows.append([NAME[r], secs(served_at(D, r)), num(s.get("first_height")), num(s.get("cpu_s")), num(s.get("server_cpu_s")),
                         gib(s.get("server_rss_peak")), gib(s.get("read")), gib(s.get("write")), gib(g(r, "index_bytes")),
                         secs(g(r, "restart_seconds")), ms(g(r, "ranges", "10000", "p50_ms")), secs(g(r, "wallet_download", "seconds"))])
    doc.table(["Server", "Time to tip", "First indexed height", "CPU s (with node)", "Server CPU s", "Server peak RSS", "Disk read", "Disk written",
               "Disk", "Restart", "10,000-block p50", "Wallet download"], rows, rows_srv=srv)

    # ---------- resources ----------
    doc.section("resources", "Resource use")
    doc.p("A sampler read <code>/proc</code> once a second for each process: CPU time, resident memory and disk bytes. "
          "For every server but ztreamer direct the totals include the standalone zakurad, which they need; in the direct run the node is inside ztreamerd.")
    doc.img("sync-resources", "Resources while syncing")
    doc.img("index-growth", "Index growth")
    doc.img("disk-footprint", "Disk footprint")
    doc.img("serving-resources", "Resources while serving")
    groups = ["idle", "ranges", "concurrency 10k", "concurrency 100", "wallet download", "ghz"]
    rows, srv = [], []
    for r in R:
        sr = D[r].get("serving_resources", {})
        for gname in groups:
            t = sr.get(gname)
            if t and t.get("seconds"):
                rows.append([NAME[r], gname, secs(t["seconds"]), num(t["cpu_s"], 1), f"{t['cpu_s'] / t['seconds']:.2f}", gib(t["rss_peak"])])
                srv.append(grp(r))
    doc.table(["Server", "Workload", "Duration", "CPU s", "Mean cores", "Peak RSS"], rows, rows_srv=srv)
    doc.p("<strong>Serving efficiency</strong>: compact blocks served per CPU second of the server and its node, in the workloads that serve blocks.")
    rows = []
    for gname in ["ranges", "concurrency 10k", "concurrency 100", "wallet download"]:
        rows.append([gname] + [num(D[r].get("efficiency", {}).get(gname, {}).get("blocks_per_cpu_s")) for r in R])
    doc.table(["Workload"] + [f"{NAME[r]} blocks / CPU s" for r in R], rows, cols=cols(R))
    doc.p(meta["resource_text"])
    doc.img("restart-memory", "Restart and memory")
    rows, srv = [], []
    for r in R:
        mem = D[r].get("memory", {})
        for k in ["server/idle", "server/load", "zakurad/idle", "zakurad/load"]:
            if k in mem:
                who, when = k.split("/")
                rows.append([NAME[r], "server" if who == "server" else "zakurad node", when, gib(mem[k]["anon"]), gib(mem[k]["file"]), gib(mem[k]["rss"])])
                srv.append(grp(r))
    doc.table(["Server", "Process", "When", "Anonymous", "File-backed", "RSS"], rows, rows_srv=srv)
    rows = [[NAME[r], secs(D[r].get("restart_seconds")), secs(D[r].get("restart_wallet_seconds")),
             num(_m((D[r].get("restart_concurrency", {}).get("64") or {}).get("blocks_per_s")), 2)] for r in R]
    doc.table(["Server", "Restart to serving", "Wallet download after restart", "M blocks / s at 64 clients after restart"], rows, rows_srv=rsrv(R))
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
            row.append(ms(x.get("p50_ms")) if x.get("status") == "ok" else f"<em>{x.get('status', '–')}</em>")
        rows.append(row)
    doc.table(["Case"] + [f"{NAME[r]} p50" for r in R], rows, cols=cols(R))
    rows = []
    for c in cases:
        rows.append([c] + [ms(D[r]["latency"].get(c, {}).get("p99_ms")) if D[r]["latency"].get(c, {}).get("status") == "ok" else "–" for r in R])
    doc.table(["Case"] + [f"{NAME[r]} p99" for r in R], rows, cols=cols(R))

    doc.section("ranges", "Range sizes")
    doc.p("GetBlockRange over 1,000 seeded random ranges per size (100 at 100,000 blocks), one client, sequential.")
    doc.img("ranges", "Range sizes")
    for metric, label in [("p50_ms", "p50"), ("p99_ms", "p99")]:
        rows = [[f"{int(s):,} blocks"] + [ms(D[r]["ranges"].get(s, {}).get(metric)) for r in R] for s in ["10", "100", "1000", "10000", "100000"]]
        doc.table(["Range"] + [f"{NAME[r]} {label}" for r in R], rows, cols=cols(R))

    doc.section("concurrency", "Concurrent serving")
    doc.p("Closed loop: each client keeps one GetBlockRange outstanding for 20 s per level, on its own connection. Two request sizes: "
          "10,000 blocks (a wallet catching up) and 100 blocks (a wallet that is nearly synced). A level fails when any request fails; "
          "its throughput is then not counted.")
    doc.img("concurrency", "Concurrency")
    for suite, label in [("concurrency-10k", "10,000 blocks per request"), ("concurrency-100", "100 blocks per request")]:
        rows = []
        for c in ["1", "4", "16", "64", "128", "256"]:
            row = [c]
            for r in R:
                x = D[r]["concurrency"][suite].get(c, {})
                bad = conc_cell(x)
                row += [bad, "–"] if bad else [num(_m(x.get("blocks_per_s")), 3), ms(x.get("p99_ms"))]
            rows.append(row)
        doc.p(f"<strong>{label}</strong>: M blocks / s and p99 completion time")
        doc.table(["Clients"] + [f"{NAME[r]} {p}" for r in R for p in ("M blocks/s", "p99")], rows, cols=cols(R, 2))
    doc.note(meta["concurrency_note"])

    doc.section("wallet-download", "Wallet download")
    doc.p("One client downloads every compact block from Sapling activation to the end of the interval, 10,000 blocks per request. "
          "This is the network part of a new wallet's first sync; it excludes trial decryption, scanning and storage.")
    doc.img("wallet-download", "Wallet download")
    rows = []
    for r in R:
        w = D[r]["wallet_download"]
        rows.append([NAME[r], secs(w.get("seconds")), num(w.get("blocks")), gib(w.get("bytes")),
                     num(w["blocks"] / w["seconds"]) if ok(w.get("seconds")) and ok(w.get("blocks")) else "–"])
    doc.table(["Server", "Time", "Blocks", "Protobuf bytes", "Blocks / s"], rows, rows_srv=rsrv(R))

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
                row += [num(x.get("rps")), ms(x.get("p50_ms")), num(x.get("errors"))]
            rows.append(row)
    doc.table(["RPC", "Workers"] + [f"{NAME[r]} {p}" for r in R for p in ("req/s", "p50", "errors")], rows, "filterable", cols(R, 3, 2))

    # ---------- wallet benchmarks ----------
    if W:
        doc.section("walletsync-bench", "Wallet sessions: zcash-walletsync-bench")
        doc.p(meta["lwb_text"])
        doc.img("walletsync-bench", "zcash-walletsync-bench")
        steps = list(dict.fromkeys(k for r in W for k in D[r].get("lwb", {})))
        rows = []
        for st in steps:
            row = [st]
            for r in W:
                x = D[r].get("lwb", {}).get(st)
                if not x:
                    row.append("–")
                    continue
                cell = tsec(x["p50"])
                lo, hi = x.get("ci_low"), x.get("ci_high")
                if ok(lo) and ok(hi):
                    cell += f" [{tsec(lo)}, {tsec(hi)}]"
                if x.get("failed"):
                    cell += f" · {x['failed']} failed"
                row.append(cell)
            rows.append(row)
        doc.table(["Step"] + [f"{NAME[r]} p50 [95 % CI]" for r in W], rows, cols=cols(W))
        rows = [[w] + [num(((D[r].get("lwb_scale") or {}).get(w) or {}).get("aggregate_mbytes_per_s"), 1) for r in W] for w in ["1", "2", "4", "8"]]
        doc.p("<strong>Scale</strong>: aggregate MB/s of parallel wallets (median).")
        doc.table(["Wallets"] + [f"{NAME[r]} MB/s" for r in W], rows, cols=cols(W))

        doc.section("cbb", "Wallets at once: cbb")
        doc.p(meta["cbb_text"])
        doc.img("cbb", "cbb")
        for mode, label in [("tree", "GetBlockRange with GetTreeState every 5 ranges"), ("blockrange", "GetBlockRange only")]:
            rows = []
            for c in ["50", "100", "150", "200"]:
                for pas in ["cold", "warm"]:
                    row = [c, pas]
                    for r in W:
                        x = D[r]["cbb"].get(f"{mode}/{c}/{pas}", {})
                        req, err, wall = x.get("requests"), x.get("errors"), x.get("wall_s")
                        good = (req - err) / wall if ok(req) and ok(err) and ok(wall) and wall else None
                        row += [num(good), ms(x.get("p50")), ms(x.get("p99")),
                                f"{num(err)} / {num(req)}" if ok(err) and ok(req) else num(err)]
                    rows.append(row)
            doc.p(f"<strong>{label}</strong>")
            doc.table(["Wallets", "Pass"] + [f"{NAME[r]} {p}" for r in W for p in ("ok req/s", "p50", "p99", "errors / requests")], rows,
                      cols=cols(W, 4, 2))

    # ---------- failures ----------
    doc.section("errors", "Failures and errors")
    doc.bullets(meta["errors"])

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
    title = [("ztreamer v0.2.0", ""), (" against ", "any"), ("lightwalletd", "lw"), (" and ", "lw zg"), ("zingod", "zg")]
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
