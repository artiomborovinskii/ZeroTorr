#!/usr/bin/env bash
# Кросс-сборка ZeroTorr в статические musl-бинарники (amd64 + arm64) через zig cc.
#
# Зависимости (без root):
#   pip3 install ziglang cargo-zigbuild        # zig как C-компилятор и линкер
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#
# Максимальное сжатие (рекомендуется): nightly + build-std
#   rustup toolchain install nightly --profile minimal --component rust-src
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl --toolchain nightly
# panic_immediate_abort вырезает форматирование паник из std: минус ~0.8–1.5 МБ.
# Нет nightly — скрипт соберёт на стабильном (чуть больше бинарник).
set -e
cd "$(dirname "$0")"

TARGETS=(
    "x86_64-unknown-linux-musl"
    "aarch64-unknown-linux-musl"
)

NIGHTLY=()
if rustup toolchain list 2>/dev/null | grep -q '^nightly' && \
   rustup component list --toolchain nightly 2>/dev/null | grep -q 'rust-src (installed)'; then
    NIGHTLY=(+nightly -Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort)
    echo "=== build-std: ON  (nightly, panic_immediate_abort) ==="
else
    echo "=== build-std: OFF (stable toolchain) ==="
fi

for T in "${TARGETS[@]}"; do
    echo "=== building $T ==="
    # std-библиотека уже в sysroot (руками или rustup) — не ходим в сеть лишний раз
    [ -d "$(rustc --print sysroot)/lib/rustlib/$T" ] || rustup target add "$T"
    # RUSTFLAGS +crypto/+sha СОЗНАТЕЛЬНО не ставятся (см. worklog v1.4.3):
    # RustCrypto 0.10 детектит SHA-NI/ARM-crypto в рантайме (cpufeatures),
    # а compile-time target-feature даёт SIGILL на CPU без крипто-расширений
    # (ТВ-боксы BRCM и пр.). Асмовый SHA1 уже подключён (sha1-wrapper).
    cargo "${NIGHTLY[@]}" zigbuild --release --target "$T"
done

# Сжатый UI: пересобрать .gz, если html правился позже
if [ assets/ui.html -nt assets/ui.html.gz ]; then
    gzip -9n -c assets/ui.html > assets/ui.html.gz
    echo "=== assets/ui.html.gz regenerated ==="
fi

# UPX-сжатие (если установлен)
UPX_BIN="$(command -v upx || true)"
[ -z "$UPX_BIN" ] && [ -x /home/z/my-project/scripts/upx ] && UPX_BIN=/home/z/my-project/scripts/upx
for T in "${TARGETS[@]}"; do
    B="target/${T}/release/zerotorr"
    [ -f "$B" ] || continue
    if [ -n "$UPX_BIN" ]; then
        cp "$B" "$B.upx"
        "$UPX_BIN" --best --lzma "$B.upx"
    fi
done

echo
echo "Готово:"
ls -lh target/*/release/zerotorr* 2>/dev/null || true
