"""Compare only isolated release owners. Never sample an installed server."""
import ctypes
import json
import math
import os
from pathlib import Path
import socket
import statistics
import subprocess
import sys
import tempfile
import time


repo = Path(__file__).resolve().parents[2]
target = repo / "target"
baseline = Path(sys.argv[1]).resolve(strict=True)
current = (target / "release/luvus").resolve(strict=True)
assert sys.platform == "darwin" and not os.environ.get("LUVUS_ENV")
assert baseline.is_relative_to(target) and baseline.relative_to(target).parts[0].startswith("app-e2e-baseline-")
assert current == target / "release/luvus"
root = Path(tempfile.mkdtemp(dir=target, prefix="app-e2e-perf-"))
lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)


def identity(pid, binary):
    path = ctypes.create_string_buffer(4096)
    assert lib.proc_pidpath(pid, path, len(path)) and Path(os.fsdecode(path.value)) == binary


def metrics(pid, binary):
    identity(pid, binary)
    # rusage_info_v2 starts with a UUID followed by the counters in sys/resource.h.
    data = (ctypes.c_uint64 * 20)()
    assert lib.proc_pid_rusage(pid, 2, ctypes.byref(data)) == 0
    fds = subprocess.check_output(["/usr/sbin/lsof", "-nP", "-p", str(pid), "-Ff"], text=True)
    threads = subprocess.check_output(["/bin/ps", "-M", "-p", str(pid), "-o", "time="], text=True)
    return dict(physical_bytes=data[9], rss_bytes=data[8], fds=sum(s[1:].isdigit() for s in fds.splitlines() if s.startswith("f")), threads=len(threads.splitlines()))


def cpu(pid):
    value = subprocess.check_output(["/bin/ps", "-o", "time=", "-p", str(pid)], text=True).strip()
    return sum(float(n) * 60**i for i,n in enumerate(reversed(value.split(":"))))


def run(binary, label, count):
    home = root / (label+str(count))
    home.mkdir(mode=0o700)
    (home / "config.json").write_text(json.dumps(dict(shell="/bin/sh", check_updates=False, remote_hosts=[])))
    env = {k:v for k,v in os.environ.items() if not k.startswith("LUVUS_")}
    env.update(HOME=str(home), LUVUS_HOME=str(home), XDG_CONFIG_HOME=str(home/"config"), GIT_CEILING_DIRECTORIES=str(root))
    def cli(*args):
        return subprocess.check_output([binary,"--session","bench",*args],env=env,cwd=home,timeout=20,text=True)
    assert "not running" in cli("server","status")
    cli("server","start")
    try:
        pid=int((home/"sessions/bench/server.pid").read_text().split()[0]); identity(pid,binary)
        record=next(s for s in json.loads(cli("session","list","--json"))["sessions"] if s["name"]=="bench")
        assert Path(record["session_dir"]) == home/"sessions/bench"
        def api(method, **params):
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(5)
                stream.connect(record["socket_path"])
                stream.sendall((json.dumps(dict(id="bench",method=method,params=params))+"\n").encode())
                reply=json.loads(stream.makefile("rb").readline(4*1024*1024))
                assert "error" not in reply, reply
                return reply["result"]
        for _ in range(count-1):
            api("tab.new")
        tabs=api("session.snapshot")["workspaces"][0]["tabs"]
        panes=[p["pane_id"] for t in tabs for p in t["panes"]]
        assert len(panes)==count
        time.sleep(1)
        before=metrics(pid,binary)
        start=time.monotonic(); c=cpu(pid); time.sleep(2)
        idle=(cpu(pid)-c)*100/(time.monotonic()-start)
        times=[]
        for n in range(12):
            t=time.monotonic()
            api("pane.send_input",pane=panes[0],text=f"printf 'measure%s\\n' ':{n}:ok'\n")
            while f"measure:{n}:ok" not in api("pane.read",pane=panes[0])["text"]:
                assert time.monotonic()-t<2
                time.sleep(.002)
            times.append((time.monotonic()-t)*1000)
        start=time.monotonic(); c=cpu(pid)
        for i in range(40):
            for pane in panes:
                api("pane.send_input",pane=pane,text=f"printf 'fixed-row-{i}\\n'\n")
            time.sleep(max(0,start+(i+1)*.05-time.monotonic()))
        elapsed=time.monotonic()-start
        return dict(pty_count=count, idle_cpu_pct=idle, active_cpu_pct=(cpu(pid)-c)*100/elapsed, active_seconds=elapsed,
                    idle=before, after=metrics(pid,binary), input_ms_p50=statistics.median(times),input_ms_p95=sorted(times)[math.ceil(.95*len(times))-1])
    finally:
        cli("server","stop")
        assert "not running" in cli("server","status")


report={"scope":"headless macOS owners, same tab geometry, 20 commands/s per PTY; latencies include control/readback", "baseline":[], "current":[]}
for count in [1,8,32]:
    for label,binary in [("baseline",baseline),("current",current)]:
        value=run(binary,label,count)
        report[label].append(value)
        print(label,value,flush=True)
        (root/"result.json").write_text(json.dumps(report,indent=2))
print(root/"result.json")
