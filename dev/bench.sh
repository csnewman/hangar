#!/bin/sh
# Benchmarks environments against each other, one at a time, from the Mac:
#
#   dev/bench.sh [-boot N] ENV...
#
# Made for comparing the same machine under Cloud Hypervisor and libkrun in
# the Lima VM (the Bench ch and Bench krun templates, the worker in
# dev/lima-krun-worker.sh beside dev/lima-worker.sh's), but any running
# environments do. Each is reached over the SSH gateway with a key of the
# script's own, out/bench/id_ed25519, which it adds to the dev stack's
# sign-in keys the first time; what each needs (fio, sysbench, iperf3,
# glmark2) is installed in it from its distribution first. The network
# tests run iperf3 against the Lima VM, which passt maps the guest's
# gateway to.
#
# -boot N also stops and starts each N times, timing the start. The raw
# output and a summary are kept in out/bench/<time>/.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
boots=0
if [ "${1:-}" = -boot ]; then
    boots=$2
    shift 2
fi
[ $# -gt 0 ] || { echo "usage: dev/bench.sh [-boot N] ENV..." >&2; exit 2; }
out=$repo/out/bench/$(date +%Y%m%d-%H%M%S)
mkdir -p "$out"
api=http://hangar.localhost:8080/api/frontend
curl_api() { curl -sf --resolve hangar.localhost:8080:127.0.0.1 -H Content-Type:application/json "$@"; }
# Its own key, so a run needs no SSH agent, and asks nothing of one.
key=$repo/out/bench/id_ed25519
if [ ! -f "$key" ]; then
    ssh-keygen -q -t ed25519 -N "" -C "dev/bench.sh" -f "$key"
fi
if ! curl_api "$api/me/profile" | grep -qF "$(cut -d' ' -f2 "$key.pub")"; then
    curl_api -X POST -o /dev/null "$api/me/profile/login-keys" \
        -d "{\"name\": \"dev/bench.sh\", \"public_key\": \"$(cat "$key.pub")\"}"
fi
ssh_opts="-o BatchMode=yes -o StrictHostKeyChecking=no -o LogLevel=ERROR -o IdentitiesOnly=yes -o IdentityAgent=none -i $key -p 2222"
ssh_env() { ssh $ssh_opts "$1@hangar.localhost" "$2"; }

env_id() {
    curl_api "$api/environments" | python3 -c '
import json, sys
for e in json.load(sys.stdin):
    if e["name"] == sys.argv[1]:
        print(e["id"])' "$1"
}

# phase waits for an environment to reach a phase, and prints how long it
# took, in seconds.
phase() {
    python3 - "$1" "$2" <<'PY'
import json, subprocess, sys, time
env, want = sys.argv[1], sys.argv[2]
start = time.time()
while time.time() - start < 600:
    out = subprocess.run(["curl", "-sf", "--resolve", "hangar.localhost:8080:127.0.0.1",
                          "http://hangar.localhost:8080/api/frontend/environments"],
                         capture_output=True, text=True).stdout
    for e in json.loads(out or "[]"):
        if e["id"] == env:
            if e["phase"] == want:
                print(f"{time.time() - start:.2f}")
                sys.exit(0)
            if e["phase"] == "failed":
                sys.exit(f"failed: {e.get('reason')}")
    time.sleep(0.25)
sys.exit("timed out")
PY
}

# The guest's half: each test's raw output after a line naming it.
guest_tests='
set -u
export DEBIAN_FRONTEND=noninteractive
missing=
for p in fio sysbench iperf3 glmark2-es2-wayland; do command -v "$p" >/dev/null || missing="$missing $p"; done
if [ -n "$missing" ]; then
    sudo -n apt-get update -qq >/dev/null && sudo -n apt-get install -y -qq $missing >/dev/null 2>&1
fi
gw=$(ip route | awk "/^default/ {print \$3; exit}")
drop() { sync; echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null; }
TIMEFORMAT=%R
echo "=== cpu1"; sysbench cpu --threads=1 --time=10 run
echo "=== cpuN"; sysbench cpu --threads=$(nproc) --time=10 run
echo "=== memory"; sysbench memory --memory-block-size=1M --memory-total-size=64G --time=10 --threads=1 run
f=/var/tmp/bench.fio
fio_run() { fio --name=bench --filename=$f --size=2G --direct=1 --ioengine=libaio --output-format=json "$@"; }
echo "=== seqwrite"; fio_run --rw=write --bs=1M --iodepth=16
echo "=== seqread"; fio_run --rw=read --bs=1M --iodepth=16
echo "=== randread"; fio_run --rw=randread --bs=4k --iodepth=32 --time_based --runtime=15
echo "=== randwrite"; fio_run --rw=randwrite --bs=4k --iodepth=32 --time_based --runtime=15
echo "=== fsync"; fio --name=fsync --filename=$f.sync --size=64M --rw=write --bs=4k --fsync=1 --ioengine=sync --output-format=json
rm -f $f $f.sync
echo "=== find"; drop; { time find /usr -xdev >/dev/null; } 2>&1
echo "=== copy"; drop; rm -rf /var/tmp/bench-copy; { time { cp -a /usr/share /var/tmp/bench-copy; sync; }; } 2>&1
echo "=== delete"; { time { rm -rf /var/tmp/bench-copy; sync; }; } 2>&1
echo "=== netdown"; iperf3 -c "$gw" -t 10 -R -J
echo "=== netup"; iperf3 -c "$gw" -t 10 -J
echo "=== gpu"; XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=wayland-0 timeout 300 glmark2-es2-wayland --off-screen -s 1280x720 \
    -b build:use-vbo=true -b texture:texture-filter=linear -b shading:shading=phong -b bump:bump-render=normals \
    -b effect2d -b pulsar -b desktop -b refract 2>&1
echo "=== end"
'

limactl shell hangar bash -c 'pkill -x iperf3; iperf3 -s -D -B 127.0.0.1 >/dev/null' 2>/dev/null || true
for env in "$@"; do
    id=$(env_id "$env")
    [ -n "$id" ] || { echo "no environment $env" >&2; exit 1; }
    echo "== $env"
    raw=$out/$env.txt
    : > "$raw"
    i=0
    while [ "$i" -lt "$boots" ]; do
        curl_api -X POST -o /dev/null "$api/environments/$id/stop"
        phase "$id" stopped > /dev/null
        curl_api -X POST -o /dev/null "$api/environments/$id/start"
        echo "=== boot" >> "$raw"
        phase "$id" running >> "$raw"
        i=$((i + 1))
    done
    # Settled, so the idle figures are its own.
    sleep 30
    echo "=== idle" >> "$raw"
    curl_api "$api/environments/$id" >> "$raw"
    echo >> "$raw"
    echo "=== ssh" >> "$raw"
    python3 - "$env" $ssh_opts >> "$raw" <<'PY'
import subprocess, sys, time
times = []
for _ in range(10):
    t = time.time()
    subprocess.run(["ssh", *sys.argv[2:], f"{sys.argv[1]}@hangar.localhost", "true"], check=True)
    times.append(time.time() - t)
times.sort()
print(f"{times[len(times) // 2]:.3f}")
PY
    ssh_env "$env" "bash -s" >> "$raw" 2>&1 <<EOF
$guest_tests
EOF
done
limactl shell hangar bash -c 'pkill -x iperf3' 2>/dev/null || true

python3 - "$out" "$@" <<'PY' | tee "$out/summary.txt"
import json, re, sys
out, envs = sys.argv[1], sys.argv[2:]

def sections(path):
    s, name = {}, None
    for line in open(path, errors="replace"):
        if line.startswith("=== "):
            name = line[4:].strip()
            s.setdefault(name, []).append("")
        elif name:
            s[name][-1] += line
    return s

def fio(text, kind):
    j = json.loads(text[text.index("{"):])["jobs"][0][kind]
    return j

def grab(pattern, text, cast=float):
    m = re.search(pattern, text)
    return cast(m.group(1)) if m else None

rows = [
    ("boot", "s", "lower", lambda s: sorted(float(x) for x in s["boot"])[len(s["boot"]) // 2] if "boot" in s else None),
    ("idle memory on the worker", "MiB", "lower", lambda s: json.loads(s["idle"][0])["stats"]["host_memory_mib"]),
    ("idle memory used by the guest", "MiB", "lower", lambda s: json.loads(s["idle"][0])["stats"]["memory_used_mib"]),
    ("ssh round trip", "s", "lower", lambda s: float(s["ssh"][0])),
    ("cpu, 1 thread", "events/s", "higher", lambda s: grab(r"events per second:\s+([\d.]+)", s["cpu1"][0])),
    ("cpu, all threads", "events/s", "higher", lambda s: grab(r"events per second:\s+([\d.]+)", s["cpuN"][0])),
    ("memory bandwidth", "MiB/s", "higher", lambda s: grab(r"\(([\d.]+) MiB/sec\)", s["memory"][0])),
    ("disk sequential write", "MiB/s", "higher", lambda s: fio(s["seqwrite"][0], "write")["bw"] / 1024),
    ("disk sequential read", "MiB/s", "higher", lambda s: fio(s["seqread"][0], "read")["bw"] / 1024),
    ("disk random read 4k", "IOPS", "higher", lambda s: fio(s["randread"][0], "read")["iops"]),
    ("disk random write 4k", "IOPS", "higher", lambda s: fio(s["randwrite"][0], "write")["iops"]),
    ("fsync'd 4k writes", "IOPS", "higher", lambda s: fio(s["fsync"][0], "write")["iops"]),
    ("find /usr, cold", "s", "lower", lambda s: float(s["find"][0].split()[-1])),
    ("copy /usr/share", "s", "lower", lambda s: float(s["copy"][0].split()[-1])),
    ("delete the copy", "s", "lower", lambda s: float(s["delete"][0].split()[-1])),
    ("network, host to guest", "Gbit/s", "higher", lambda s: json.loads(s["netdown"][0])["end"]["sum_received"]["bits_per_second"] / 1e9),
    ("network, guest to host", "Gbit/s", "higher", lambda s: json.loads(s["netup"][0])["end"]["sum_received"]["bits_per_second"] / 1e9),
    ("GPU (glmark2, llvmpipe)", "score", "higher", lambda s: grab(r"glmark2 Score:\s+(\d+)", s["gpu"][0])),
]

data = {e: sections(f"{out}/{e}.txt") for e in envs}
results = {}
width = max(len(r[0]) for r in rows) + 2
print(f"{'':{width}}" + "".join(f"{e:>16}" for e in envs) + "   unit")
for name, unit, better, get in rows:
    vals = []
    for e in envs:
        try:
            v = get(data[e])
        except Exception:
            v = None
        vals.append(v)
        results.setdefault(e, {})[name] = v
    cells = "".join(f"{v:>16.2f}" if isinstance(v, (int, float)) else f"{'-':>16}" for v in vals)
    note = ""
    if len(vals) == 2 and all(isinstance(v, (int, float)) and v for v in vals):
        a, b = vals
        ratio = b / a if better == "higher" else a / b
        note = f"   {envs[1]} {ratio:.2f}x"
    print(f"{name:{width}}{cells}   {unit}{note}")
json.dump(results, open(f"{out}/results.json", "w"), indent=2)
PY
echo "kept in $out"
