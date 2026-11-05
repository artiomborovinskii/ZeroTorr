#!/data/data/com.termux/files/usr/bin/bash
# Сборка ZeroTorr прямо в Termux (Android, aarch64).
#
# Причина типовой ошибки сборки:
#   error: failed to run custom build command for `ring v0.17.14`
#   ... "aarch64-linux-android-gcc": No such file or directory
# Крейт ring (TLS) компилирует C-код и по умолчанию ищет NDK-компилятор
# с именем aarch64-linux-android-gcc. В Termux его нет — нужно указать
# clang/ar из Termux (см. экспорт ниже).
#
# Установка зависимостей (один раз):
#   pkg update && pkg install rust clang binutils git
#
# Запуск сборки:
#   bash build-termux.sh
# Результат:
#   target/aarch64-linux-android/release/zerotorr
#
# Примечание: этот бинарник динамический (bionic, библиотеки Termux) и
# предназначен для запуска внутри Termux — DNS там работает штатно.
# Для обычного Linux (proot/Alpine/серверы) используйте статический
# musl-бинарник из релизов или ./build-static.sh.

set -e
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust не найден. Установите: pkg install rust clang binutils"
    exit 1
fi

# --- Ключевой фикс: C-компилятор и архиватор для цели android ---
export CC_aarch64_linux_android=clang
export AR_aarch64_linux_android=ar
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=clang

echo "=== сборка aarch64-linux-android (Termux) ==="
cargo build --release --target aarch64-linux-android

BIN=target/aarch64-linux-android/release/zerotorr
echo
echo "Готово: $BIN"
ls -lh "$BIN"
echo
echo "Запуск: $BIN -p 8090 -d ~/zerotorr-data"
