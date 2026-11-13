#!/usr/bin/env python3
"""Builds the data, plots and headline numbers of the 2026-11 benchmark from its run directories.

    report.py --run ztreamer-direct=DIR --run ztreamer-remote=DIR --run lightwalletd=DIR --out OUT

OUT gets data/*.csv (all runs, one label column), one SVG and one PNG per plot, and
numbers.json, which the site page and the written report read. It never runs a benchmark.
"""
import argparse, csv, json, math, os, re, statistics
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib import font_manager
import numpy as np

os.environ.setdefault("SOURCE_DATE_EPOCH", "1794528000")
REPO = Path(__file__).resolve().parents[3]
SITE = REPO / "site"
TOK = dict(re.findall(r"--([a-z0-9-]+):(#[0-9a-fA-F]{6})", (SITE / "site.css").read_text().split("}", 1)[0]))
INK, DIM, BG, FAINT = TOK["ink"], TOK["dim"], TOK["bg"], TOK["faint"]
ORDER = ["ztreamer-direct", "ztreamer-remote", "lightwalletd"]
NAME = {"ztreamer-direct": "ztreamer direct", "ztreamer-remote": "ztreamer remote", "lightwalletd": "lightwalletd"}
COLOR = {"ztreamer-direct": TOK["series-v010"], "ztreamer-remote": TOK["series-zinder"], "lightwalletd": TOK["series-v001"]}
for f in ("JetBrainsMono-Regular.ttf", "Sora-Bold.ttf"):
    font_manager.fontManager.addfont(str(REPO / "scripts/fonts" / f))
plt.rcParams.update({
    "font.family": "JetBrains Mono", "svg.hashsalt": "ztreamer", "font.size": 10, "axes.titlesize": 12,
    "axes.titlepad": 12, "text.color": INK, "axes.labelcolor": DIM, "xtick.color": DIM, "ytick.color": DIM,
    "axes.edgecolor": TOK["chart-axis"], "figure.facecolor": BG, "axes.facecolor": BG, "savefig.facecolor": BG,
    "grid.color": INK, "grid.alpha": .07, "legend.facecolor": BG, "legend.edgecolor": TOK["rule"],
    "svg.fonttype": "path",
})


def read_csv(p):
    return list(csv.DictReader(open(p))) if Path(p).exists() else []


def fnum(x):
    try:
        v = float(x)
        return v if math.isfinite(v) else None
    except (TypeError, ValueError):
        return None


class Run:
    """One run directory of run-direct.sh, run-remote.sh or run-lwd.sh."""

    def __init__(self, label, d, server=None):
        self.label, self.d = label, Path(d)
        self.env = dict(l.split("=", 1) for l in (self.d / "env.txt").read_text().splitlines() if "=" in l)
        self.phases = {r["phase"]: (float(r["start_unix_s"]), float(r["end_unix_s"])) for r in read_csv(self.d / "phases.csv")}
        self.res = read_csv(self.d / "resources.csv")
        self.disk = read_csv(self.d / "resources-disk.csv")
        self.server = server or ("lightwalletd" if label == "lightwalletd" else "ztreamerd")
        self.summary, self.samples = [], []
        for s in sorted((self.d / "serving").glob("*/summary.csv")):
            sub = s.parent.name
            for r in read_csv(s):
                r["run_suite"] = sub
                self.summary.append(r)
            for r in read_csv(s.parent / "samples.csv"):
                if r["phase"] == "measure":
                    r["run_suite"] = sub
                    self.samples.append(r)
        self.ghz = {}
        for f in sorted((self.d / "serving/ghz").glob("*.json")):
            m = re.match(r"(.+)-c(\d+)$", f.stem)
            try:
                self.ghz[(m.group(1), int(m.group(2)))] = json.load(open(f))
            except (json.JSONDecodeError, AttributeError):
                pass
        self.synced = {}
        if (self.d / "synced.txt").exists():
            self.synced = dict(x.split("=") for x in (self.d / "synced.txt").read_text().split())
        self.gnu_time = {}
        if (self.d / f"{self.server}.time").exists():
            for line in (self.d / f"{self.server}.time").read_text().splitlines():
                if ":" in line:
                    k, v = line.rsplit(":", 1) if "Elapsed" not in line else line.split(": ", 1)
                    self.gnu_time[k.strip()] = v.strip()

    def proc_rows(self, proc, t0=None, t1=None):
        return [r for r in self.res if r["process"] == proc and (t0 is None or float(r["unix_s"]) >= t0) and (t1 is None or float(r["unix_s"]) <= t1)]

    def procs(self):
        return [self.server] + (["zakurad"] if any(r["process"] == "zakurad" for r in self.res) else [])

    def window_stats(self, t0, t1, procs=None):
        """CPU seconds, peak and mean RSS, and disk bytes of the given processes in [t0, t1]."""
        out = dict(cpu_s=0.0, rss_peak=0, rss_mean=0.0, read=0, write=0)
        for p in procs or self.procs():
            rows = self.proc_rows(p, t0, t1)
            if len(rows) < 2:
                continue
            a, b = rows[0], rows[-1]
            out["cpu_s"] += (float(b["cpu_user_s"]) + float(b["cpu_sys_s"])) - (float(a["cpu_user_s"]) + float(a["cpu_sys_s"]))
            out["read"] += int(b["read_bytes"]) - int(a["read_bytes"])
            out["write"] += int(b["write_bytes"]) - int(a["write_bytes"])
            rss = [int(r["rss_bytes"]) for r in rows]
            out["rss_peak"] += max(rss)
            out["rss_mean"] += statistics.mean(rss)
        return out

    def phase_stats(self, name, procs=None):
        if name not in self.phases:
            return None
        t0, t1 = self.phases[name]
        s = self.window_stats(t0, t1, procs)
        s["seconds"] = t1 - t0
        return s

    def sync_curve(self):
        """(seconds since the sync phase began, indexed height) of the server."""
        if "sync" not in self.phases:
            return [], []
        t0, t1 = self.phases["sync"]
        xs, ys = [], []
        for r in self.proc_rows(self.server, t0, t1 + 5):
            h = fnum(r["indexed_height"])
            if h is not None:
                xs.append(float(r["unix_s"]) - t0)
                ys.append(h)
        return xs, ys

    def row(self, run_suite, case=None, clients=None):
        for r in self.summary:
            if r["run_suite"] == run_suite and (case is None or r["case"] == case) and (clients is None or int(r["clients"]) == clients):
                return r
        return None

    def val(self, run_suite, metric, case=None, clients=None, scale=1.0):
        r = self.row(run_suite, case, clients)
        if not r or r["status"] != "ok":
            return np.nan
        v = fnum(r.get(metric))
        return v / scale if v is not None else np.nan

    def ghz_stat(self, rpc, c, key):
        g = self.ghz.get((rpc, c))
        if not g:
            return np.nan
        if key == "rps":
            # Successful requests per second of the whole run.
            ok = (g.get("statusCodeDistribution") or {}).get("OK", 0)
            return ok / (g["total"] / 1e9) if g.get("total") else np.nan
        if key == "errors":
            # ghz cancels the calls in flight at the deadline; those are not server errors.
            return sum(v for k, v in (g.get("errorDistribution") or {}).items() if not artifact(k))
        if key in ("readers_full", "node_429"):
            pat = "MDB_READERS_FULL" if key == "readers_full" else "429"
            return sum(v for k, v in (g.get("errorDistribution") or {}).items() if pat in k)
        if key == "ok":
            return (g.get("statusCodeDistribution") or {}).get("OK", 0)
        for d in g.get("latencyDistribution") or []:
            if d["percentage"] == key:
                return d["latency"] / 1e6  # ns to ms
        if key == "average":
            return g.get("average", np.nan) / 1e6
        return np.nan


def artifact(error):
    """True for the errors that ghz makes itself when it stops at the deadline."""
    return "client connection is closing" in error or "use of closed network connection" in error


def figure(title, subtitle, nrows=1, ncols=2, size=(15, 6.5), runs=()):
    fig, axes = plt.subplots(nrows, ncols, figsize=size, squeeze=False)
    fig.subplots_adjust(left=.085, right=.97, bottom=.12, top=.975 - 1.55 / size[1], hspace=.5, wspace=.28)
    fig.suptitle(title, x=.06, y=.975, ha="left", fontsize=22, fontfamily="Sora", fontweight="bold")
    fig.text(.06, .975 - .5 / size[1], subtitle, ha="left", color=DIM, fontsize=10)
    if runs:
        fig.legend(handles=[plt.Line2D([0], [0], color=COLOR[r.label], marker="o", lw=2, label=NAME[r.label]) for r in runs],
                   loc="upper left", bbox_to_anchor=(.055, .975 - .62 / size[1]), frameon=False, ncol=3)
    return fig, axes.ravel()


def plain(v, _pos=None):
    if v == 0:
        return "0"
    a = abs(v)
    if a >= 1e6:
        return f"{v / 1e6:g}M"
    if a >= 1e3:
        return f"{v / 1e3:g}k"
    return f"{v:g}"


def plain_log(ax, axis="y"):
    from matplotlib.ticker import FuncFormatter, LogLocator, NullFormatter
    a = ax.yaxis if axis == "y" else ax.xaxis
    a.set_major_locator(LogLocator(base=10, subs=(1.0, 2.0, 5.0)))
    a.set_major_formatter(FuncFormatter(plain))
    a.set_minor_formatter(NullFormatter())


def style(ax, title, ylabel="", log=False, xlabel=""):
    ax.set_title(title, loc="left")
    ax.set_ylabel(ylabel)
    ax.set_xlabel(xlabel)
    ax.grid(axis="y")
    ax.set_axisbelow(True)
    if log:
        ax.set_yscale("log")
        plain_log(ax)


def fmt_ms(v):
    if not np.isfinite(v):
        return "–"
    return f"{v * 1000:.0f} µs" if v < 1 else (f"{v:.2f} ms" if v < 100 else (f"{v:.0f} ms" if v < 1000 else f"{v / 1000:.2f} s"))


class Report:
    def __init__(self, runs, out, restarts=()):
        self.runs, self.out, self.restarts = runs, Path(out), list(restarts)
        (self.out / "data").mkdir(parents=True, exist_ok=True)
        self.numbers = {"runs": {}}

    def save(self, fig, name):
        fig.savefig(self.out / f"{name}.svg", bbox_inches="tight")
        fig.savefig(self.out / f"{name}.png", bbox_inches="tight", dpi=110)
        plt.close(fig)

    # ---------- data export ----------
    def export(self):
        def dump(name, rows):
            if not rows:
                return
            keys = list(dict.fromkeys(k for r in rows for k in r))
            with open(self.out / "data" / name, "w", newline="") as f:
                w = csv.DictWriter(f, keys)
                w.writeheader()
                w.writerows(rows)
        dump("summary.csv", [dict(label=r.label, **{k: v for k, v in x.items() if k != "label"}) for r in self.runs for x in r.summary])
        # The 100-block concurrency suite has about 2 M samples; its rows stay in the run directories,
        # and its summaries are in summary.csv.
        dump("samples.csv", [dict(label=r.label, **{k: v for k, v in x.items() if k != "label"}) for r in self.runs for x in r.samples
                             if x["run_suite"] != "concurrency-100"])
        dump("resources.csv", [dict(label=r.label, **x) for r in self.runs for x in r.res])
        dump("disk.csv", [dict(label=r.label, **x) for r in self.runs for x in r.disk])
        dump("phases.csv", [dict(label=r.label, phase=k, start_unix_s=a, end_unix_s=b) for r in self.runs for k, (a, b) in r.phases.items()])
        ghz_rows = []
        for r in self.runs:
            for (rpc, c), g in sorted(r.ghz.items()):
                ghz_rows.append(dict(label=r.label, rpc=rpc, concurrency=c, count=g.get("count"), ok=r.ghz_stat(rpc, c, "ok"), ok_per_second=r.ghz_stat(rpc, c, "rps"),
                                     errors=r.ghz_stat(rpc, c, "errors"), readers_full=r.ghz_stat(rpc, c, "readers_full"), node_429=r.ghz_stat(rpc, c, "node_429"), rps=g.get("rps"), average_ms=r.ghz_stat(rpc, c, "average"),
                                     **{f"p{p}_ms": r.ghz_stat(rpc, c, p) for p in (10, 25, 50, 75, 90, 95, 99)},
                                     status_codes=json.dumps(g.get("statusCodeDistribution")), error_sample=json.dumps(list((g.get("errorDistribution") or {}).keys())[:3])))
        dump("ghz.csv", ghz_rows)
        self.ghz_rows = ghz_rows

    # ---------- sync ----------
    def sync(self):
        fig, a = figure("Sync speed", "From process start to the indexed tip of a synced zakura node · same machine · one run each",
                        1, 2, (15, 6.5), self.runs)
        for r in self.runs:
            xs, ys = r.sync_curve()
            if xs:
                a[0].plot(np.maximum(np.array(xs), 1), np.array(ys) / 1e6, color=COLOR[r.label], lw=2.3)
        style(a[0], "Indexed height over time", "Million blocks", xlabel="Seconds since process start · log scale")
        a[0].set_xscale("log")
        plain_log(a[0], "x")
        totals = []
        for r in self.runs:
            st = r.phase_stats("sync") or {}
            totals.append(st.get("seconds", np.nan))
        a[1].barh(range(len(self.runs)), totals, color=[COLOR[r.label] for r in self.runs])
        a[1].set_yticks(range(len(self.runs)), [NAME[r.label] for r in self.runs])
        a[1].invert_yaxis()
        a[1].set_xscale("log")
        plain_log(a[1], "x")
        for i, t in enumerate(totals):
            if np.isfinite(t):
                a[1].text(t * 1.08, i, human_s(t), va="center", color=INK)
        a[1].set_xlim(right=np.nanmax(totals) * 4)
        style(a[1], "Time to synced and serving ↓", xlabel="Seconds · log scale")
        self.save(fig, "sync-speed")

        # Blocks per second by height band.
        fig, a = figure("Sync rate by height", "Blocks per second in each 100,000-block band of the indexed height", 1, 1, (15, 6), self.runs)
        for r in self.runs:
            xs, ys = r.sync_curve()
            if len(xs) < 3:
                continue
            band = 100_000
            edges = np.arange(0, max(ys) + band, band)
            t_at = np.interp(edges, ys, xs, left=np.nan, right=np.nan)
            rate = band / np.diff(t_at)
            a[0].step(edges[:-1] / 1e6, rate, where="post", color=COLOR[r.label], lw=2)
        style(a[0], "Blocks per second ↑", "Blocks / s · log scale", True, "Height · millions")
        self.save(fig, "sync-rate")

        for r in self.runs:
            st = r.phase_stats("sync") or {}
            xs, ys = r.sync_curve()
            n = self.numbers["runs"].setdefault(r.label, {})
            n["sync"] = dict(seconds=st.get("seconds"), cpu_s=st.get("cpu_s"), server_cpu_s=(r.phase_stats("sync", [r.server]) or {}).get("cpu_s"),
                             rss_peak=st.get("rss_peak"), server_rss_peak=(r.phase_stats("sync", [r.server]) or {}).get("rss_peak"),
                             read=st.get("read"), write=st.get("write"), height=ys[-1] if ys else None,
                             first_height=ys[0] if ys else None,
                             blocks_per_s=(ys[-1] / st["seconds"]) if ys and st.get("seconds") else None)
            n["sync"].update(self.log_milestones(r))

    def log_milestones(self, r):
        """Times in the server log, relative to the start of the sync phase."""
        out = {}
        if "sync" not in r.phases:
            return out
        t0 = r.phases["sync"][0]
        log = r.d / f"{r.server}.log"
        if not log.exists():
            return out
        from datetime import datetime
        pats = {"historical_complete": "historical compact index complete", "import_complete": "import complete",
                "import_start": "importing epochs", "serving": "serving CompactTxStreamer", "remote_ready": "remote zakura is ready"}
        for line in open(log, errors="replace"):
            line = re.sub(r"\x1b\[[0-9;]*m", "", line)
            for k, p in pats.items():
                if k not in out and p in line:
                    m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z", line)
                    if m:
                        ts = datetime.fromisoformat(m.group(1) + "+00:00").timestamp()
                        out[k + "_s"] = ts - t0
                    h = re.search(r"height=(\d+)", line)
                    if h:
                        out[k + "_height"] = int(h.group(1))
                    b = re.search(r"bytes=(\d+)", line)
                    if b and k == "import_start":
                        out["import_bytes"] = int(b.group(1))
        return out

    # ---------- resources ----------
    def resources(self):
        fig, a = figure("Resources while syncing", "Sampled each second from /proc · server and its zakura node · cores = CPU seconds per second",
                        2, 2, (15, 10.5), self.runs)
        for r in self.runs:
            if "sync" not in r.phases:
                continue
            t0, t1 = r.phases["sync"]
            for proc, ls in [(r.server, "-"), ("zakurad", ":")]:
                rows = r.proc_rows(proc, t0, t1)
                if len(rows) < 3:
                    continue
                t = np.array([float(x["unix_s"]) - t0 for x in rows])
                cpu = np.array([float(x["cpu_user_s"]) + float(x["cpu_sys_s"]) for x in rows])
                rss = np.array([int(x["rss_bytes"]) for x in rows]) / 2**30
                rd = np.array([int(x["read_bytes"]) for x in rows]) / 2**20
                wr = np.array([int(x["write_bytes"]) for x in rows]) / 2**20
                dt = np.maximum(np.diff(t), 1e-3)
                k = max(1, len(t) // 300) if len(t) > 600 else 3
                sm = lambda v: np.convolve(np.diff(v) / dt, np.ones(k) / k, mode="same")
                lab = NAME[r.label] + ("" if proc == r.server else " · node")
                t = np.maximum(t, 1)
                a[0].plot(t[1:], sm(cpu), color=COLOR[r.label], ls=ls, lw=1.4, label=lab)
                a[1].plot(t, rss, color=COLOR[r.label], ls=ls, lw=1.8, label=lab)
                a[2].plot(t[1:], sm(rd), color=COLOR[r.label], ls=ls, lw=1.2, label=lab)
                a[3].plot(t[1:], sm(wr), color=COLOR[r.label], ls=ls, lw=1.2, label=lab)
        for ax, t, y in zip(a, ["CPU", "Resident memory", "Disk reads", "Disk writes"], ["Cores", "GiB", "MiB / s", "MiB / s"]):
            style(ax, t, y, xlabel="Seconds since process start · log scale")
            ax.set_xscale("log")
            plain_log(ax, "x")
        a[1].legend(loc="upper left", fontsize=8, frameon=False)
        self.save(fig, "sync-resources")

        # Serving resources per phase group.
        groups = [("latency", "suite:latency"), ("ranges", "suite:ranges"), ("concurrency 10k", "suite:concurrency-10k"),
                  ("concurrency 100", "suite:concurrency-100"), ("wallet download", "suite:sync"), ("ghz", "ghz:")]
        fig, a = figure("Resources while serving", "Server and node CPU seconds and peak RSS for each workload · idle = 60 s after sync",
                        1, 2, (15, 6.8), self.runs)
        w = .8 / len(self.runs)
        names = ["idle"] + [g for g, _ in groups]
        table = {}
        for i, r in enumerate(self.runs):
            cpu, rss = [], []
            for g, prefix in [("idle", "idle:warm")] + groups:
                spans = [v for k, v in r.phases.items() if k == prefix or (prefix.endswith(":") and k.startswith(prefix))]
                if not spans:
                    cpu.append(np.nan); rss.append(np.nan); continue
                tot = dict(cpu_s=0.0, rss_peak=0, seconds=0.0, read=0, write=0)
                for t0, t1 in spans:
                    s = r.window_stats(t0, t1)
                    tot["cpu_s"] += s["cpu_s"]; tot["rss_peak"] = max(tot["rss_peak"], s["rss_peak"])
                    tot["seconds"] += t1 - t0; tot["read"] += s["read"]; tot["write"] += s["write"]
                table.setdefault(r.label, {})[g] = tot
                cpu.append(tot["cpu_s"] / tot["seconds"] if tot["seconds"] else np.nan)
                rss.append(tot["rss_peak"] / 2**30)
            x = np.arange(len(names)) + (i - (len(self.runs) - 1) / 2) * w
            a[0].bar(x, cpu, w, color=COLOR[r.label])
            a[1].bar(x, rss, w, color=COLOR[r.label])
        for ax in a:
            ax.set_xticks(range(len(names)), names, rotation=20, ha="right")
        style(a[0], "Mean CPU while the workload runs", "Cores")
        style(a[1], "Peak resident memory", "GiB · server + node")
        self.save(fig, "serving-resources")
        suites = {"concurrency 10k": "concurrency-10k", "concurrency 100": "concurrency-100", "wallet download": "sync", "ranges": "ranges"}
        for r in self.runs:
            n = self.numbers["runs"].setdefault(r.label, {})
            n["serving_resources"] = table.get(r.label, {})
            eff = {}
            for g, suite in suites.items():
                t = table.get(r.label, {}).get(g)
                blocks = sum(fnum(x.get("successful_blocks")) or 0 for x in r.summary if x["run_suite"] == suite)
                if t and t["cpu_s"] > 0:
                    eff[g] = dict(blocks=blocks, cpu_s=t["cpu_s"], blocks_per_cpu_s=blocks / t["cpu_s"])
            n["efficiency"] = eff

        # Disk footprint.
        fig, a = figure("Disk footprint", "Size of the index or cache that each server keeps · the zakura state is shared and not counted",
                        1, 1, (15, 4.6), self.runs)
        sizes = []
        for r in self.runs:
            f = r.d / "index-size.txt"
            sizes.append(int(f.read_text().split()[0]) / 2**30 if f.exists() else np.nan)
            self.numbers["runs"].setdefault(r.label, {})["index_bytes"] = int(f.read_text().split()[0]) if f.exists() else None
        a[0].barh(range(len(self.runs)), sizes, color=[COLOR[r.label] for r in self.runs])
        a[0].set_yticks(range(len(self.runs)), [NAME[r.label] for r in self.runs])
        a[0].invert_yaxis()
        for i, s in enumerate(sizes):
            if np.isfinite(s):
                a[0].text(s + .2, i, f"{s:.1f} GiB", va="center", color=INK)
        a[0].set_xlim(0, np.nanmax(sizes) * 1.2)
        style(a[0], "On-disk size ↓", xlabel="GiB")
        self.save(fig, "disk-footprint")

        # Index growth during sync.
        fig, a = figure("Index growth while syncing", "du -sb of the index directory, every 15 s", 1, 1, (15, 5.2), self.runs)
        for r in self.runs:
            if "sync" not in r.phases:
                continue
            t0, t1 = r.phases["sync"]
            pts = [(float(x["unix_s"]) - t0, int(x["bytes"]) / 2**30) for x in r.disk if x["bytes"] and t0 <= float(x["unix_s"]) <= t1 + 30]
            if pts:
                a[0].plot([max(p[0], 1) for p in pts], [p[1] for p in pts], color=COLOR[r.label], lw=2.2, marker=".")
        style(a[0], "Index size", "GiB", xlabel="Seconds since process start · log scale")
        a[0].set_xscale("log")
        plain_log(a[0], "x")
        self.save(fig, "index-growth")

    # ---------- serving: native suite ----------
    def overview(self):
        R = self.runs
        fig, a = figure("Serving: ztreamer and lightwalletd", "Native Rust/Tonic client · fixed interval 419,200 to 3,564,614 · loopback · sequential runs", 2, 2, (15, 10.5), R)
        sizes = [10, 100, 1000, 10000, 100000]
        clients = [1, 4, 16, 64, 128, 256]
        for r in R:
            a[0].plot(range(len(sizes)), [r.val("ranges", "complete_p50_us", f"GetBlockRange/{s}", scale=1000) for s in sizes], color=COLOR[r.label], marker="o", lw=2.3)
            a[1].plot(range(len(clients)), [r.val("concurrency-10k", "blocks_per_second", clients=c, scale=1e6) for c in clients], color=COLOR[r.label], marker="o", lw=2.3)
            a[2].plot(range(len(clients)), [r.val("concurrency-10k", "complete_p99_us", clients=c, scale=1e6) for c in clients], color=COLOR[r.label], marker="o", lw=2.3)
        a[0].set_xticks(range(len(sizes)), ["10", "100", "1k", "10k", "100k"])
        for ax in a[1:3]:
            ax.set_xticks(range(len(clients)), clients)
        style(a[0], "GetBlockRange median latency ↓", "Milliseconds · log scale", True, "Blocks per request")
        style(a[1], "Concurrent throughput ↑", "Million blocks / s", xlabel="Concurrent clients · 10,000 blocks / request")
        style(a[2], "Concurrent p99 latency ↓", "Seconds · log scale", True, "Concurrent clients · 10,000 blocks / request")
        times = [r.val("sync", "wall_seconds") for r in R]
        a[3].barh(range(len(R)), times, color=[COLOR[r.label] for r in R])
        a[3].set_yticks(range(len(R)), [NAME[r.label] for r in R])
        a[3].invert_yaxis()
        for i, t in enumerate(times):
            if np.isfinite(t):
                a[3].text(t * 1.02, i, human_s(t), va="center", color=INK)
        a[3].set_xlim(0, np.nanmax(times) * 1.3)
        style(a[3], "Wallet download, Sapling to tip ↓", xlabel="Seconds · compact blocks only, no trial decryption")
        self.save(fig, "overview")

    def latencies(self):
        R = self.runs
        cases = [x["case"] for x in R[0].summary if x["run_suite"] == "latency"]
        fig, a = figure("RPC latency, every measured method", "500 sequential requests per case · completion includes stream draining · log scale", 1, 2, (16, 12), R)
        fig.subplots_adjust(left=.27, right=.97, bottom=.07, top=.975 - 1.55 / 12, wspace=.12)
        for ax, metric, title in zip(a, ["complete_p50_us", "complete_p99_us"], ["p50 ↓", "p99 ↓"]):
            for i, c in enumerate(cases):
                ys = [r.val("latency", metric, c, scale=1000) for r in R]
                ok = [y for y in ys if np.isfinite(y)]
                if ok:
                    ax.plot([min(ok), max(ok)], [i, i], color=FAINT, lw=1.2)
                for r, y in zip(R, ys):
                    ax.plot(y, i, "o", color=COLOR[r.label], ms=7)
            ax.set_yticks(range(len(cases)), cases if ax is a[0] else [""] * len(cases))
            ax.invert_yaxis()
            ax.set_xscale("log")
            plain_log(ax, "x")
            ax.grid(axis="x")
            ax.set_title(title, loc="left")
            ax.set_xlabel("Milliseconds · log scale")
        self.save(fig, "rpc-latencies")

    def ranges(self):
        R = self.runs
        sizes = [10, 100, 1000, 10000, 100000]
        fig, a = figure("GetBlockRange by range size", "1,000 random ranges per size (100 at 100k) · sequential · one client", 2, 2, (15, 10.5), R)
        for r in R:
            for ax, metric, scale in [(a[0], "complete_p50_us", 1000), (a[1], "complete_p99_us", 1000), (a[2], "first_p50_us", 1000), (a[3], "blocks_per_second", 1)]:
                ax.plot(range(len(sizes)), [r.val("ranges", metric, f"GetBlockRange/{s}", scale=scale) for s in sizes], color=COLOR[r.label], marker="o", lw=2.3)
        for ax in a:
            ax.set_xticks(range(len(sizes)), ["10", "100", "1k", "10k", "100k"])
            ax.set_xlabel("Blocks per request")
        style(a[0], "Complete, p50 ↓", "Milliseconds · log", True)
        style(a[1], "Complete, p99 ↓", "Milliseconds · log", True)
        style(a[2], "First block, p50 ↓", "Milliseconds · log", True)
        style(a[3], "Single-stream rate ↑", "Blocks / s · log", True)
        self.save(fig, "ranges")

    def concurrency(self):
        R = self.runs
        clients = [1, 4, 16, 64, 128, 256]
        fig, a = figure("Concurrent GetBlockRange", "Closed loop · one outstanding request per client · 20 s per level · 10,000 and 100 blocks per request", 2, 3, (17, 10.5), R)
        for row, suite in enumerate(["concurrency-10k", "concurrency-100"]):
            for r in R:
                a[row * 3 + 0].plot(range(6), [r.val(suite, "blocks_per_second", clients=c, scale=1e3) for c in clients], color=COLOR[r.label], marker="o", lw=2.2)
                a[row * 3 + 1].plot(range(6), [r.val(suite, "complete_p50_us", clients=c, scale=1e3) for c in clients], color=COLOR[r.label], marker="o", lw=2.2)
                a[row * 3 + 2].plot(range(6), [r.val(suite, "complete_p99_us", clients=c, scale=1e3) for c in clients], color=COLOR[r.label], marker="o", lw=2.2)
            n = "10k" if suite.endswith("10k") else "100"
            style(a[row * 3], f"{n} blocks: throughput ↑", "Thousand blocks / s · log", True)
            style(a[row * 3 + 1], f"{n} blocks: p50 ↓", "Milliseconds · log", True)
            style(a[row * 3 + 2], f"{n} blocks: p99 ↓", "Milliseconds · log", True)
        for ax in a:
            ax.set_xticks(range(6), clients)
            ax.set_xlabel("Concurrent clients")
        self.save(fig, "concurrency")

    def wallet(self):
        R = self.runs
        fig, a = figure("Wallet download", "One client downloads every compact block from Sapling activation to the tip, 10,000 blocks per request", 1, 2, (15, 6.5), R)
        for r in R:
            rows = [x for x in r.samples if x["run_suite"] == "sync" and x["status"] == "ok"]
            if not rows:
                continue
            t = np.cumsum([float(x["complete_us"]) / 1e6 for x in rows])
            h = [int(x["end_height"]) / 1e6 for x in rows]
            a[0].plot(t, h, color=COLOR[r.label], lw=2.3)
            rate = [int(x["blocks"]) / (float(x["complete_us"]) / 1e6) / 1e3 for x in rows]
            a[1].plot(h, rate, color=COLOR[r.label], lw=1.4)
        style(a[0], "Height reached", "Million blocks", xlabel="Seconds")
        style(a[1], "Chunk rate ↑", "Thousand blocks / s · log", True, "Height · millions")
        self.save(fig, "wallet-download")
        for r in R:
            n = self.numbers["runs"].setdefault(r.label, {})
            n["wallet_download"] = dict(seconds=r.val("sync", "wall_seconds"), bytes=fnum((r.row("sync") or {}).get("successful_protobuf_bytes")),
                                        blocks=fnum((r.row("sync") or {}).get("successful_blocks")))

    # ---------- serving: ghz ----------
    def ghz(self):
        R = self.runs
        rpcs = list(dict.fromkeys(rpc for r in R for (rpc, _) in sorted(r.ghz, key=lambda k: k[0]))) if R else []
        order = ["GetLatestBlock", "GetLightdInfo", "GetBlock", "GetBlockNullifiers", "GetTreeState", "GetLatestTreeState",
                 "GetTransaction", "GetTaddressBalance", "GetSubtreeRoots", "GetBlockRange", "GetBlockRangeNullifiers"]
        rpcs = [x for x in order if x in rpcs]
        cs = sorted({c for r in R for (_, c) in r.ghz})
        if not rpcs:
            return
        ncol = 4
        nrow = math.ceil(len(rpcs) / ncol)
        fig, a = figure("Throughput per RPC", "ghz · closed loop · one connection per worker · 15 s per level · successful requests per second · ring = over 1 % failed",
                        nrow, ncol, (17, 3.9 * nrow + 1.4), R)
        for ax, rpc in zip(a, rpcs):
            for r in R:
                ys = [r.ghz_stat(rpc, c, "rps") for c in cs]
                ax.plot(range(len(cs)), ys, color=COLOR[r.label], marker="o", lw=2)
                for i, c in enumerate(cs):
                    ok_, err = r.ghz_stat(rpc, c, "ok"), r.ghz_stat(rpc, c, "errors")
                    if np.isfinite(err) and err > .01 * max(1, ok_ + err):
                        ax.plot(i, ys[i], marker="o", ms=13, mfc="none", mec=INK, mew=1.5)
            ax.set_xticks(range(len(cs)), cs)
            style(ax, rpc, "", True)
        for ax in a[len(rpcs):]:
            ax.axis("off")
        self.save(fig, "ghz-throughput")
        fig, a = figure("Latency per RPC under load", "ghz · p50 (dot) and p99 (bar end) at 1 and 64 concurrent workers · milliseconds · log scale", 1, 2, (16, 10), R)
        fig.subplots_adjust(left=.2, right=.97, bottom=.08, top=.975 - 1.55 / 10, wspace=.1)
        for ax, c in zip(a, [1, 64]):
            for i, rpc in enumerate(rpcs):
                for j, r in enumerate(R):
                    y = i + (j - 1) * .25
                    p50, p99 = r.ghz_stat(rpc, c, 50), r.ghz_stat(rpc, c, 99)
                    if np.isfinite(p50):
                        ax.plot([p50, p99], [y, y], color=COLOR[r.label], lw=2, alpha=.6)
                        ax.plot(p50, y, "o", color=COLOR[r.label], ms=6)
            ax.set_yticks(range(len(rpcs)), rpcs if ax is a[0] else [""] * len(rpcs))
            ax.invert_yaxis()
            ax.set_xscale("log")
            plain_log(ax, "x")
            ax.grid(axis="x")
            ax.set_title(f"{c} concurrent worker{'s' if c > 1 else ''}", loc="left")
            ax.set_xlabel("Milliseconds")
        self.save(fig, "ghz-latency")

    # ---------- restart and memory ----------
    def restart(self):
        R = self.restarts
        if not R:
            return
        fig, a = figure("Restart and memory", "Each server restarted on its finished index · RSS split into anonymous (heap) and file-backed (mapped index) pages",
                        1, 2, (15, 6.8), R)
        times = [(r.phase_stats("restart") or {}).get("seconds", np.nan) for r in R]
        a[0].barh(range(len(R)), times, color=[COLOR[r.label] for r in R])
        a[0].set_yticks(range(len(R)), [NAME[r.label] for r in R])
        a[0].invert_yaxis()
        for i, t in enumerate(times):
            if np.isfinite(t):
                a[0].text(t * 1.02, i, human_s(t), va="center", color=INK)
        a[0].set_xlim(0, np.nanmax(times) * 1.3)
        style(a[0], "Restart to serving ↓", xlabel="Seconds · process start to synced and serving")
        labels, anon, filed, colors = [], [], [], []
        for r in R:
            for ph, pname in [("idle:warm", "idle"), ("suite:concurrency-10000", "load")]:
                if ph not in r.phases:
                    continue
                t0, t1 = r.phases[ph]
                for proc in r.procs():
                    rows = [x for x in r.proc_rows(proc, t0, t1) if x.get("rss_anon_bytes")]
                    if not rows:
                        continue
                    top = max(rows, key=lambda x: int(x["rss_bytes"]))
                    labels.append(f"{NAME[r.label]}{' node' if proc == 'zakurad' else ''} · {pname}")
                    anon.append(int(top["rss_anon_bytes"]) / 2**30)
                    filed.append(int(top["rss_file_bytes"]) / 2**30)
                    colors.append(COLOR[r.label])
                    self.numbers["runs"].setdefault(r.label, {}).setdefault("memory", {})[f"{proc if proc == 'zakurad' else 'server'}/{pname}"] = dict(
                        anon=int(top["rss_anon_bytes"]), file=int(top["rss_file_bytes"]), rss=int(top["rss_bytes"]))
        y = np.arange(len(labels))
        a[1].barh(y, anon, color=colors)
        a[1].barh(y, filed, left=anon, color=colors, alpha=.35, hatch="//", edgecolor=BG)
        a[1].set_yticks(y, labels, fontsize=8)
        a[1].invert_yaxis()
        for i in range(len(labels)):
            a[1].text(anon[i] + filed[i] + .2, i, f"{anon[i]:.2f} + {filed[i]:.2f} GiB", va="center", color=INK, fontsize=8)
        a[1].set_xlim(0, max(x + f for x, f in zip(anon, filed)) * 1.35)
        style(a[1], "Peak RSS: anonymous (solid) + file-backed (hatched)", xlabel="GiB")
        a[1].grid(axis="x")
        fig.subplots_adjust(left=.13, wspace=.45)
        self.save(fig, "restart-memory")
        for r in R:
            n = self.numbers["runs"].setdefault(r.label, {})
            n["restart_seconds"] = (r.phase_stats("restart") or {}).get("seconds")
            n["restart_concurrency"] = {c: dict(blocks_per_s=r.val("concurrency", "blocks_per_second", clients=c), p99_ms=r.val("concurrency", "complete_p99_us", clients=c, scale=1e3)) for c in [1, 16, 64, 256]}
            n["restart_wallet_seconds"] = r.val("sync", "wall_seconds")
            s = r.phase_stats("suite:concurrency-10000")
            if s:
                n["restart_load_cpu_cores"] = s["cpu_s"] / s["seconds"]

    # ---------- numbers ----------
    def headline(self):
        N = self.numbers
        for r in self.runs:
            n = N["runs"].setdefault(r.label, {})
            n["env"] = r.env
            n["ranges"] = {s: dict(p50_ms=r.val("ranges", "complete_p50_us", f"GetBlockRange/{s}", scale=1e3), p99_ms=r.val("ranges", "complete_p99_us", f"GetBlockRange/{s}", scale=1e3),
                                   first_p50_ms=r.val("ranges", "first_p50_us", f"GetBlockRange/{s}", scale=1e3), blocks_per_s=r.val("ranges", "blocks_per_second", f"GetBlockRange/{s}"))
                           for s in [10, 100, 1000, 10000, 100000]}
            n["concurrency"] = {suite: {c: dict(blocks_per_s=r.val(suite, "blocks_per_second", clients=c), p50_ms=r.val(suite, "complete_p50_us", clients=c, scale=1e3),
                                                p99_ms=r.val(suite, "complete_p99_us", clients=c, scale=1e3), errors=fnum((r.row(suite, clients=c) or {}).get("errors")))
                                        for c in [1, 4, 16, 64, 128, 256]} for suite in ["concurrency-10k", "concurrency-100"]}
            n["latency"] = {x["case"]: dict(p50_ms=fnum(x["complete_p50_us"]) / 1e3 if fnum(x["complete_p50_us"]) else None,
                                            p99_ms=fnum(x["complete_p99_us"]) / 1e3 if fnum(x["complete_p99_us"]) else None, status=x["status"], errors=x["errors"])
                            for x in r.summary if x["run_suite"] == "latency"}
            n["ghz"] = {f"{rpc}/{c}": dict(rps=r.ghz_stat(rpc, c, "rps"),
                                           p50_ms=r.ghz_stat(rpc, c, 50), p99_ms=r.ghz_stat(rpc, c, 99), errors=r.ghz_stat(rpc, c, "errors"),
                                           readers_full=r.ghz_stat(rpc, c, "readers_full"), node_429=r.ghz_stat(rpc, c, "node_429"),
                                           ok=r.ghz_stat(rpc, c, "ok"))
                        for (rpc, c) in r.ghz}
            n["gnu_time"] = r.gnu_time
            n["phases"] = {k: dict(seconds=b - a) for k, (a, b) in r.phases.items()}
        with open(self.out / "numbers.json", "w") as f:
            json.dump(clean(N), f, indent=1, allow_nan=False)

    def build(self):
        self.export()
        self.sync()
        self.resources()
        self.overview()
        self.latencies()
        self.ranges()
        self.concurrency()
        self.wallet()
        self.ghz()
        self.restart()
        self.headline()


def human_s(t):
    if t < 120:
        return f"{t:.1f} s"
    if t < 7200:
        return f"{t / 60:.1f} min"
    return f"{t / 3600:.2f} h"


def clean(v):
    if isinstance(v, float) and not math.isfinite(v):
        return None
    if isinstance(v, dict):
        return {str(k): clean(x) for k, x in v.items()}
    if isinstance(v, list):
        return [clean(x) for x in v]
    if isinstance(v, (np.floating,)):
        return clean(float(v))
    if isinstance(v, (np.integer,)):
        return int(v)
    return v


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--run", action="append", required=True, help="LABEL=DIR")
    p.add_argument("--restart", action="append", default=[], help="LABEL=DIR of run-restart.sh")
    p.add_argument("--out", required=True)
    a = p.parse_args()
    runs = dict(x.split("=", 1) for x in a.run)
    rs = dict(x.split("=", 1) for x in a.restart)
    rep = Report([Run(l, runs[l]) for l in ORDER if l in runs], a.out,
                 [Run(l, rs[l], server="server") for l in ORDER if l in rs])
    rep.build()


if __name__ == "__main__":
    main()
