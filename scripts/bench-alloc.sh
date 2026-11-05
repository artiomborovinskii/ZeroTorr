#!/bin/bash
# A/B-бенчмарк аллокаторов ZeroTorr: musl mallocng (без фичи) vs jemalloc
# (--features jemalloc). Одинаковая нагрузка на каждую сборку: локальный сидер
# + подопытный в --low-memory, полный стрим 32 МиБ, семплирование VmRSS
# каждую секунду во время стрима и 12 с после (скорость возврата памяти ОС).
#
# Использование:  bench-alloc.sh <bin1> [bin2 ...]  (x86_64-musl, нативно)
# Сборки перемежаются по кругу (run1: bin1,bin2; run2: bin1,bin2 …) — дрейф
# фона не искажает сравнение.
set -u
export PATH="$HOME/.cargo/bin:$PATH"
ROOT=/tmp/zt-bench
SEEDER=/home/z/my-project/zerotorr/target/x86_64-unknown-linux-musl/release/zerotorr
RUNS="${RUNS:-2}"
PORT_A=8095
PORT_B=8096

BINS=("$@")
[ ${#BINS[@]} -eq 0 ] && { echo "нет бинарников для сравнения"; exit 1; }
for b in "${BINS[@]}"; do [ -x "$b" ] || { echo "нет исполняемого: $b"; exit 1; }; done

rm -rf $ROOT; mkdir -p $ROOT/a
echo '{"PeersListenPort": 41995}' > $ROOT/a/settings.json
python3 /home/z/my-project/scripts/mktorrent.py $ROOT 33554432 >/dev/null
HASH=$(cat $ROOT/infohash.txt)
MD5=$(cat $ROOT/payload.md5)
echo "== infohash $HASH"

stat_of() { curl -s -X POST http://127.0.0.1:$1/torrents -d '{"action":"list"}' | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    ts=d['torrents'] if isinstance(d,dict) else d
    print(','.join(f\"{t['stat']}:{t['loaded_size']}/{t['torrent_size']}\" for t in ts) if ts else 'EMPTY')
except Exception as e:
    print('ERR', e)"; }

run_once() { # $1 = бинарник под теста, $2 = метка
  local BIN="$1" TAG="$2"
  rm -rf /dev/shm/zt-bench-b; mkdir -p $ROOT/b
  "$SEEDER" -d $ROOT/a -p $PORT_A --no-ram-storage > $ROOT/a.log 2>&1 &
  local APID=$!
  "$BIN" -d /dev/shm/zt-bench-b -p $PORT_B --low-memory > $ROOT/b.log 2>&1 &
  local BPID=$!
  sleep 1.5
  curl -s -X POST http://127.0.0.1:$PORT_A/torrents \
    -d "{\"action\":\"add\",\"link\":\"file://$ROOT/test.torrent\",\"save_to_db\":false}" -o /dev/null
  curl -s -X POST http://127.0.0.1:$PORT_B/torrents \
    -d "{\"action\":\"add\",\"link\":\"magnet:?xt=urn:btih:$HASH&x-peers=127.0.0.1:41995\",\"save_to_db\":false}" -o /dev/null
  local st=""
  for i in $(seq 1 40); do
    st=$(stat_of $PORT_B); case "$st" in 3:*) break;; esac; sleep 1
  done
  sleep 3
  local rsslog=$ROOT/rss-$TAG-$RANDOM.log
  ( for i in $(seq 1 90); do
      awk '/VmRSS/{print $2}' /proc/$BPID/status 2>/dev/null || exit 0
      sleep 0.5
    done ) > $rsslog &
  local SAMPLER=$!
  curl -s -o $ROOT/out.bin --max-time 240 "http://127.0.0.1:$PORT_B/stream/1?link=$HASH&index=1"
  local GOT=$(md5sum $ROOT/out.bin | cut -d' ' -f1)
  sleep 12                      # спад: аллокатор должен вернуть страницы ОС
  kill $SAMPLER 2>/dev/null; wait $SAMPLER 2>/dev/null
  local STATS=$(sort -n $rsslog | awk '{a[++n]=$1} END{printf "%d %d %d", a[1], a[int(n/2)+0], a[n]}')
  local BASE=$(head -1 $rsslog)
  local SETTLE=$(tail -1 $rsslog)
  local OK=FAIL; [ "$GOT" = "$MD5" ] && OK=OK
  printf "%s run_rss_kib base=%s min/med/max=%s settle=%s md5=%s\n" "$TAG" "$BASE" "$STATS" "$SETTLE" "$OK"
  kill $APID $BPID 2>/dev/null; wait $APID $BPID 2>/dev/null
  rm -rf /dev/shm/zt-bench-b $ROOT/b $ROOT/out.bin
}

r=1
while [ $r -le $RUNS ]; do
  for b in "${BINS[@]}"; do
    run_once "$b" "$(basename $b)-r$r"
  done
  r=$((r+1))
done
echo "== DONE"
