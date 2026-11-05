#!/bin/bash
set -e

echo "[*] Сборка ZeroTorr с компиляцией std под musl и разгоном Cortex-A53..."

export RUSTFLAGS="-C target-cpu=cortex-a53 -C target-feature=+crypto,+crc,+aes,+sha2"

# Включаем ночной флаг build-std через RUSTC_BOOTSTRAP=1 на стабильном rustc:
RUSTC_BOOTSTRAP=1 cargo zigbuild --release \
  --target aarch64-unknown-linux-musl \
  -Z build-std=std,panic_abort

echo "[+] Сборка завершена успешно!"
ls -lh target/aarch64-unknown-linux-musl/release/zerotorr
