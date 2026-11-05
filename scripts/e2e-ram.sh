#!/bin/bash
# E2E-тест RAM-режима ZeroTorr v1.4.0 (всё в одном вызове: процессы умирают
# вместе с сессией шелла).
set -u
export PATH="$HOME/.cargo/bin:$PATH"
ROOT=/tmp/zt-e2e
B=/home/z/my-project/zerotorr/target/x86_64-unknown-linux-musl/release/zerotorr
PORT_A=8091
PORT_B=8092

rm -rf $ROOT /dev/shm/zt-b
pkill -f 'zerotorr -d' 2>/dev/null; sleep 0.5
mkdir -p $ROOT/a
echo '{"PeersListenPort": 41991}' > $ROOT/a/settings.json
python3 /home/z/my-project/scripts/mktorrent.py $ROOT
HASH=$(cat $ROOT/infohash.txt)
MD5=$(cat $ROOT/payload.md5)
echo "== infohash $HASH md5 $MD5"

$B -d $ROOT/a -p $PORT_A --no-ram-storage > $ROOT/a.log 2>&1 &
APID=$!
$B -d /dev/shm/zt-b -p $PORT_B --low-memory --dbg > $ROOT/b.log 2>&1 &
BPID=$!
sleep 1.5

curl -s -X POST http://127.0.0.1:$PORT_A/torrents \
  -d "{\"action\":\"add\",\"link\":\"file://$ROOT/test.torrent\",\"save_to_db\":false}" -o /dev/null
curl -s -X POST http://127.0.0.1:$PORT_B/torrents \
  -d "{\"action\":\"add\",\"link\":\"magnet:?xt=urn:btih:$HASH&x-peers=127.0.0.1:41991\",\"save_to_db\":false}" -o /dev/null

stat_of() { curl -s -X POST http://127.0.0.1:$1/torrents -d '{"action":"list"}' | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    ts=d['torrents'] if isinstance(d,dict) else d
    print(','.join(f\"{t['stat']}:{t['loaded_size']}/{t['torrent_size']}:p{t['total_peers']}\" for t in ts) if ts else 'EMPTY')
except Exception as e:
    print('ERR', e)"; }

for i in $(seq 1 40); do
  SA=$(stat_of $PORT_A); SB=$(stat_of $PORT_B)
  echo "t=${i}s A=[$SA] B=[$SB]"
  case "$SB" in 3:*) break;; esac
  sleep 1
done
# даём пирам и окну перекачки устаканиться (без паузы стрим может стартовать
# раньше, чем восстанавливается связь с сидером — редкий гоночный EOF)
sleep 3

echo "== stream whole file from B (RSS sampler on)"
( for i in $(seq 1 120); do
    awk '/VmRSS/{print "rss_kib", $2}' /proc/$BPID/status 2>/dev/null || exit 0
    sleep 1
  done ) > $ROOT/rss.log &
SAMPLER=$!
curl -s -o $ROOT/out.bin --max-time 240 "http://127.0.0.1:$PORT_B/stream/1?link=$HASH&index=1"
kill $SAMPLER 2>/dev/null
GOT=$(md5sum $ROOT/out.bin | cut -d' ' -f1)
echo "out.bin size: $(stat -c%s $ROOT/out.bin), md5=$GOT"
echo "expected    : size=33554432, md5=$MD5"
[ "$GOT" = "$MD5" ] && echo "CONTENT OK" || echo "CONTENT MISMATCH"
echo "== RSS samples: min/median/max KiB:"
sort -k2 -n $ROOT/rss.log | awk 'NR==1{min=$2} {a[++n]=$2} END{printf "%d / %d / %d\n", min, a[int(n/2)], a[n]}'
echo "== final B loaded_size (ring window):"; stat_of $PORT_B
echo "== /dev/shm/zt-b:"; du -sk /dev/shm/zt-b; find /dev/shm/zt-b -type f | head -5
echo "== B log tail:"; tail -12 $ROOT/b.log
kill $APID $BPID 2>/dev/null
wait 2>/dev/null
echo "== DONE"
