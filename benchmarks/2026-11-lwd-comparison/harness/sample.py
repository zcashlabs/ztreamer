#!/usr/bin/env python3
"""Resource and progress sampler for one benchmark run.

Every --interval seconds it writes one CSV row per watched process: CPU seconds (user and
system), RSS, peak RSS, threads, and disk bytes read and written (from /proc/PID/io). It also
writes the indexed height, read from the Prometheus endpoint of ztreamerd or from the log of
lightwalletd, and every 15 s the disk use of the --du directories.

Watched processes are given as NAME=PIDFILE. A PID file can appear later; the sampler waits.
The sampler stops when --stop-file exists.
"""
import argparse, csv, os, re, subprocess, sys, time, urllib.request

CLK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")


def read_pid(path):
    try:
        return int(open(path).read().strip())
    except (OSError, ValueError):
        return None


def proc_stats(pid):
    try:
        stat = open(f"/proc/{pid}/stat").read()
        fields = stat[stat.rindex(")") + 2 :].split()
        utime, stime = int(fields[11]) / CLK, int(fields[12]) / CLK
        threads = int(fields[17])
        status = open(f"/proc/{pid}/status").read()
        rss = int(re.search(r"VmRSS:\s+(\d+)", status).group(1)) * 1024
        hwm = int(re.search(r"VmHWM:\s+(\d+)", status).group(1)) * 1024
        anon = int(re.search(r"RssAnon:\s+(\d+)", status).group(1)) * 1024
        file_ = int(re.search(r"RssFile:\s+(\d+)", status).group(1)) * 1024
        io = dict(l.split(": ") for l in open(f"/proc/{pid}/io").read().splitlines())
        return dict(
            cpu_user_s=utime,
            cpu_sys_s=stime,
            rss_bytes=rss,
            rss_peak_bytes=hwm,
            rss_anon_bytes=anon,
            rss_file_bytes=file_,
            threads=threads,
            read_bytes=int(io["read_bytes"]),
            write_bytes=int(io["write_bytes"]),
        )
    except (OSError, AttributeError, ValueError, KeyError):
        return None


def ztreamer_height(url):
    try:
        text = urllib.request.urlopen(url, timeout=2).read().decode()
    except OSError:
        return ""
    m = re.search(r"^ztreamer_index_durable_block_height(?:\{[^}]*\})?\s+(\S+)", text, re.M)
    return m.group(1) if m and m.group(1) != "NaN" else ""


class LwdLog:
    """Reads the newest height from the lightwalletd log, incrementally."""

    PAT = re.compile(r"Adding block to cache (\d+)|Waiting for block: (\d+)")

    def __init__(self, path):
        self.path, self.pos, self.height = path, 0, ""

    def __call__(self):
        try:
            with open(self.path, errors="replace") as f:
                f.seek(self.pos)
                for line in f:
                    m = self.PAT.search(line)
                    if m:
                        self.height = m.group(1) or str(int(m.group(2)) - 1)
                self.pos = f.tell()
        except OSError:
            pass
        return self.height


def du(path):
    try:
        out = subprocess.run(["du", "-sb", path], capture_output=True, text=True, timeout=60)
        return out.stdout.split()[0] if out.returncode == 0 else ""
    except (subprocess.TimeoutExpired, IndexError):
        return ""


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--out", required=True)
    p.add_argument("--proc", action="append", default=[], help="NAME=PIDFILE")
    p.add_argument("--ztreamer-metrics", help="http://host:port/metrics")
    p.add_argument("--lwd-log")
    p.add_argument("--du", action="append", default=[], help="NAME=DIR")
    p.add_argument("--interval", type=float, default=1.0)
    p.add_argument("--stop-file", required=True)
    a = p.parse_args()
    procs = [x.split("=", 1) for x in a.proc]
    dus = [x.split("=", 1) for x in a.du]
    lwd = LwdLog(a.lwd_log) if a.lwd_log else None
    cols = ["unix_s", "process", "pid", "cpu_user_s", "cpu_sys_s", "rss_bytes", "rss_peak_bytes",
            "rss_anon_bytes", "rss_file_bytes", "threads", "read_bytes", "write_bytes", "indexed_height"]
    du_out = open(a.out.replace(".csv", "-disk.csv"), "w", newline="")
    du_w = csv.writer(du_out)
    du_w.writerow(["unix_s", "name", "bytes"])
    last_du = 0.0
    with open(a.out, "w", newline="") as f:
        w = csv.DictWriter(f, cols)
        w.writeheader()
        while not os.path.exists(a.stop_file):
            now = time.time()
            height = ztreamer_height(a.ztreamer_metrics) if a.ztreamer_metrics else (lwd() if lwd else "")
            for name, pidfile in procs:
                pid = read_pid(pidfile)
                s = proc_stats(pid) if pid else None
                if s:
                    w.writerow(dict(unix_s=f"{now:.3f}", process=name, pid=pid, indexed_height=height, **s))
            f.flush()
            if dus and now - last_du >= 15:
                last_du = now
                for name, d in dus:
                    du_w.writerow([f"{now:.3f}", name, du(d)])
                du_out.flush()
            time.sleep(max(0.0, a.interval - (time.time() - now)))
    du_out.close()


if __name__ == "__main__":
    sys.exit(main())
