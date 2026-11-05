#!/usr/bin/env python3
"""Полный A/B прогон: RSS-кривая всего жизненного цикла B + фазы."""
import subprocess, time, os, sys, shutil, json, urllib.request

BIN = sys.argv[1]; ROOT = sys.argv[2]; PA, PB = int(sys.argv[3]), int(sys.argv[4])
SHM = f"/dev/shm/zt-{PB}"
for d in (ROOT, SHM):
    shutil.rmtree(d, ignore_errors=True)
os.makedirs(f"{ROOT}/a", exist_ok=True)
json.dump({"PeersListenPort": PB + 33000}, open(f"{ROOT}/a/settings.json", "w"))
subprocess.run(["python3", "/home/z/my-project/scripts/mktorrent.py", ROOT],
               capture_output=True, check=True)
HASH = open(f"{ROOT}/infohash.txt").read().strip()
MD5 = open(f"{ROOT}/payload.md5").read().strip()

def post(port, body):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/torrents",
                                 data=body.encode(), method="POST")
    return urllib.request.urlopen(req, timeout=30).read().decode()[:120]

def cache_state():
    try:
        req = urllib.request.Request(f"http://127.0.0.1:{PB}/cache",
                                     data=b'{"action":"get","hash":"%s"}' % HASH,
                                     method="POST")
        d = json.loads(urllib.request.urlopen(req, timeout=5).read())
        return f"filled={d.get('Filled',0)/1048576:.1f}M pieces={d.get('PiecesCount',0)}"
    except Exception as e:
        return f"cache-err"

rss_samples = []  # (t, phase, kib)
phase = "idle"
a = subprocess.Popen([BIN, "-d", f"{ROOT}/a", "-p", str(PA), "--no-ram-storage"],
                     stdout=open(f"{ROOT}/a.log", "w"), stderr=subprocess.STDOUT)
b = subprocess.Popen([BIN, "-d", SHM, "-p", str(PB), "--low-memory"],
                     stdout=open(f"{ROOT}/b.log", "w"), stderr=subprocess.STDOUT)
t0 = time.time()
phase = "boot"
time.sleep(1.5)
phase = "add"
post(PA, '{"action":"add","link":"file://%s/test.torrent","save_to_db":false}' % ROOT)
post(PB, '{"action":"add","link":"magnet:?xt=urn:btih:%s&x-peers=127.0.0.1:%d","save_to_db":false}' % (HASH, PB + 33000))
phase = "download"
time.sleep(4)
phase = "stream"
t_s = time.time()
urllib.request.urlretrieve(f"http://127.0.0.1:{PB}/stream/1?link={HASH}&index=1", f"{ROOT}/out.bin")
phase = "after"
import hashlib
got = hashlib.md5(open(f"{ROOT}/out.bin", "rb").read()).hexdigest()
print(f"stream: {time.time()-t_s:.2f}s, content {'OK' if got == MD5 else 'FAIL'}")
hw = 0
for line in open(f"/proc/{b.pid}/status"):
    if line.startswith("VmHWM"):
        hw = int(line.split()[1])
print(f"VmHWM: {hw} KiB")
for i in range(600):
    try:
        kib = 0
        for line in open(f"/proc/{b.pid}/status"):
            if line.startswith("VmRSS"):
                kib = int(line.split()[1])
        rss_samples.append((round(time.time() - t0, 2), phase, kib))
    except Exception:
        break
    time.sleep(0.05)
a.kill(); b.kill()
peak = max(rss_samples, key=lambda x: x[2])
print(f"samples={len(rss_samples)} peak={peak[2]} KiB @ t={peak[0]}s phase={peak[1]}")
print("кривая (раз в 0.5с):")
for t, ph, k in rss_samples[::10]:
    print(f"  t={t:6.2f}s {ph:9s} {k:6d} KiB {cache_state() if ph in ('download','stream') else ''}")
