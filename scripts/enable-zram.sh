#!/bin/sh
# Включение zram-свопа на Yandex Station Lite (kernel 4.9.68).
# Ядро: CONFIG_ZRAM=y, CONFIG_ZSMALLOC=y, сжатие lzo (lz4/zstd в ядре НЕТ).
# zram = настоящее "сжатие оперативки": неактивные страницы уходят в
# сжатый пул в RAM. 64 МБ свопа занимает обычно ~25-30 МБ физической RAM.
#
# Запуск:  sh enable-zram.sh       -> 64 МБ
#          sh enable-zram.sh 96    -> 96 МБ

MB="${1:-64}"
BYTES=$((MB * 1024 * 1024))

# --- если zram уже был включён, сначала выключить и сбросить ---
if grep -q zram /proc/swaps 2>/dev/null; then
  echo "zram уже включён — выключаю и пересоздаю..."
  swapoff /dev/zram0 2>/dev/null
fi
if [ "$(cat /sys/block/zram0/disksize 2>/dev/null)" != "0" ] && [ -n "$(cat /sys/block/zram0/disksize 2>/dev/null)" ]; then
  echo 1 > /sys/block/zram0/reset 2>/dev/null
fi

# --- алгоритм сжатия (только lzo доступен в этом ядре) ---
echo "Доступные алгоритмы: $(cat /sys/block/zram0/comp_algorithm 2>/dev/null)"
echo lzo > /sys/block/zram0/comp_algorithm 2>/dev/null

# --- размер (виртуальный; физической RAM тратится только на сжатые данные) ---
echo "$BYTES" > /sys/block/zram0/disksize || { echo "FAIL: disksize"; exit 1; }
[ -b /dev/zram0 ] || mknod /dev/zram0 b 253 0

# --- mkswap + swapon (высокий приоритет: быстрее любого дискового свопа) ---
mkswap /dev/zram0 || { echo "FAIL: mkswap"; exit 1; }
swapon -p 100 /dev/zram0 2>/dev/null || swapon /dev/zram0 || { echo "FAIL: swapon"; exit 1; }

echo
echo "=== zram swap ВКЛЮЧЁН: ${MB} МБ виртуальных, сжатие lzo ==="
cat /proc/swaps
cat /sys/block/zram0/mm_stat 2>/dev/null | awk '{printf "данных %d МБ -> занято RAM %d МБ (сжатие %.1fx)\n", $1/1048576, $2/1048576, ($2>0)?$1/$2:0}'
free -m
