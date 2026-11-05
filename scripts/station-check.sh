#!/bin/sh
# Диагностика ядра Yandex Station Lite: поддержка zram/zswap/KSM + память.
# Запуск:  sh station-check.sh   (или просто вставить в терминал по частям)
# Вывод скопировать целиком обратно в чат.

echo "=== kernel ==="
uname -a

echo "=== kernel config: zram / zswap / KSM / squashfs / compressors ==="
zcat /proc/config.gz 2>/dev/null | grep -E 'CONFIG_(ZRAM|ZSWAP|ZSMALLOC|KSM|SQUASHFS|BLK_DEV_LOOP|CRYPTO_LZ4|CRYPTO_LZ4HC|CRYPTO_LZO|CRYPTO_ZSTD|CRYPTO_DEFLATE)[= ]' || echo "(config.gz недоступен или ничего не найдено)"

echo "=== squashfs/loop devices ==="
ls -l /dev/loop* 2>/dev/null | head -3 || echo "(нет /dev/loop*)"

echo "=== zram devices ==="
ls /sys/block 2>/dev/null
ls -l /dev/zram* 2>/dev/null || echo "(нет /dev/zram*)"

echo "=== swap tools ==="
which mkswap swapon 2>/dev/null
swapon 2>&1 | head -2
cat /proc/swaps

echo "=== KSM ==="
ls /sys/kernel/mm/ksm 2>/dev/null && cat /sys/kernel/mm/ksm/run 2>/dev/null || echo "(KSM нет)"

echo "=== crypto algos загруженные ==="
grep -E '^name|^driver' /proc/crypto 2>/dev/null | head -30

echo "=== module tools ==="
which modprobe insmod 2>/dev/null || echo "(нет modprobe/insmod)"
ls /lib/modules 2>/dev/null | head -5

echo "=== memory ==="
grep -E 'MemTotal|MemFree|MemAvailable|Shmem:|Slab:|SReclaimable|SUnreclaim' /proc/meminfo

echo "=== mounts (tmpfs/ubi) ==="
mount | grep -E 'tmpfs|ubi'

echo "=== df ==="
df -h

echo "=== mount capability test ==="
mkdir -p /tmp/zt-mnttest
if mount -t tmpfs -o size=1m tmpfs /tmp/zt-mnttest 2>/dev/null; then
  umount /tmp/zt-mnttest; echo "mount tmpfs: OK"
else
  echo "mount tmpfs: FAIL (mount запрещён ядром/seccomp)"
fi

echo "=== DONE ==="
