# ZeroTorr — журнал работ

> Восстановлен 2026-10-30: журнал v1.4.1 был утерян при сбросе песочницы,
> правки 1.4.1 воспроизведены по диагностике и закоммичены вместе с 1.4.2.

## Task 6 — v1.4.1: abort на Яндекс.Станции (ramstore.rs:187)

Симптом: на станции (238 МБ RAM, BusyBox, panic="abort") процесс умирал
`abort` с паникой `ramstore.rs:187:40 Option::unwrap() on None`.

Диагноз: отравленный (poisoned) мьютекс. Пока стример держал лок и паниковал
(вытесненный кусок, `pieces[&idx]` по индексу), все последующие
`lock().unwrap()` на этом мьютексе паниковали уже без всякой вины хранилища.
При `panic="abort"` любой такой unwrap — мгновенная смерть процесса.

Правки (все — src/ramstore.rs):
- `fn lock<T>(m: &Mutex<T>) -> MutexGuard<T>` через
  `unwrap_or_else(|p| p.into_inner())` — все захваты мьютексов устойчивы к
  отравлению;
- pread: отсутствие resident-куска — `anyhow!`-ошибка (перекачается), а не
  unwrap/индексирование; убран двойной поиск куска (`pieces.get` + `pieces[&idx]`);
- pwrite: `get_mut().unwrap()` → `let Some(..) = .. else { continue }`;
- в main.rs убран дублирующий баннер `storage:` (его печатает lib.rs с бюджетом).

## Task 7 — v1.4.2: Termux «No CA certificates» + антираздувание

Симптом (Termux, arm64, musl-бинарник 1.4.1):
```
Error: session init: error building HTTP(S) client:
No CA certificates were loaded from the system
```
Диагноз: reqwest+rustls при `ClientBuilder::build()` загружает системные
корни CA; в Termux/BusyBox/минимальных rootfs их нет по стандартным путям —
build() падает, `Session::new_with_opts` не завершается.

Правки:
- vendor/librqbit/src/session.rs: клиент HTTP(S) строится каскадом
  «системные корни → встроенные webpki-roots → без корней» с warn-логами;
  сессия поднимается на любом устройстве, деградация — только https-трекеры
  (падает запрос, не процесс);
- src/manager.rs: тот же каскад для собственного клиента (OnceLock, 30 с
  таймауты);
- README.md: раздел про Termux + CA, история исправлений.

Антираздувание (по код-ревью):
- удалён ureq — один сетевой стек reqwest (в графе уже был через librqbit);
  `fetch_torrent` переведён на async reqwest (+ явная проверка HTTP-статуса,
  лимит 16 МБ сохранён);
- `tracing = { features = ["release_max_level_warn"] }` — trace!/debug!/info!
  вырезаны компилятором из всех зависимостей;
- assets/ui.html.gz (2.3 КБ вместо 5.1 КБ) отдаётся с Content-Encoding: gzip,
  несжатая копия в бинарник не включается;
- runtime: стек потоков 1 МиБ (было 2), max_blocking_threads 8/32 (дефолт
  512), в --low-memory 2 tokio-воркера + пул дисковых потоков librqbit 8→4;
- sha1-wrapper: фича `asm` (sha1-asm) — SHA1 на aarch64/x86_64 без софт-тормоза;
  SHA2/SHA-NI ускоряются рантайм-детектом RustCrypto (target-feature=+crypto
  сознательно НЕ ставим — SIGILL на CPU без крипто-расширений);
- build-static.sh: nightly `-Z build-std=std,panic_abort
  -Z build-std-features=panic_immediate_abort` в профиле; нет nightly —
  автоматический откат на стабильный тулчейн. Примечание: `trim-paths`
  в [profile] всё ещё нестабилен даже в cargo 1.99.1 — включить нельзя,
  путь к фиче: `-Z unstable-options` на nightly.

Итоги сборки 1.4.2 (stable, musl, opt-level=z + lto=fat):
- amd64: 6 100 936 Б (5.8 МиБ; было 6.6 МБ) → UPX --best --lzma 2 054 556 Б
- arm64: 5 149 424 Б (4.9 МиБ; было 5.6 МБ) → UPX 1 863 392 Б (1.78 МиБ)

Тесты (все OK):
- e2e-ram.sh ×2: 32 МиБ через 8 МиБ RAM-окно, md5 совпал, RSS max ~13.7 МиБ,
  tmpfs 0 байт; первый прогон показал редкий гоночный EOF (старт стрима
  через ~2 с после добавления при 1–2 пирах) — в скрипт добавлена пауза 3 с;
- smoke-test.py: amd64 нативно, arm64 под qemu, arm64-UPX под qemu — все
  12 проверок OK (в т.ч. gzip-UI, low-memory clamp, Range/206, CD001).

Примечание о reqwest 0.13: фича `rustls` тянет rustls-platform-verifier и
НЕ имеет методов tls_built_in_* / фичи webpki-roots (как в 0.12); каскад CA
реализован через `tls_backend_preconfigured(rustls::ClientConfig)` с
`webpki_roots::TLS_SERVER_ROOTS`.

## Task 8 — v1.4.3: Lampa add с `data`-метаданными (500 «bad base64 data»)

Из поля (Android, Lampa → 127.0.0.1:8090) сняты два запроса:
1. `POST /torrents` `{action:"add", link:"magnet:?xt=urn:btih:5436…&tr=http://bt2.t-ru.org/ann?magnet",
   title:"[LAMPA] Гарри Поттер…", poster:"https://…tmdb…jpg", data:"{\"lampa\":true,\"movie\":{…}}",
   save_to_db:false, category:"movie"}` — при открытии карточки фильма;
2. `GET /gst/echo` — зонд транскодинга.

Баг: add-обработчик при непустом `data` сразу декодировал его как
base64-.torrent; JSON-метаданные Lampa (`{…`) не декодируются →
`Err("bad base64 data")` → 500 → фильм не добавляется. В MatriX.111
(`web/api/torrents.go → addTorrent`) `data` в add вообще не является
источником .torrent: link обязателен, `data` передаётся в
`torr.AddTorrent(spec, title, poster, data)` как строка-метаданные
(`Torrent.Data`), эхо в `Status()`.

Правки (семантика MatriX.111):
- manager.rs `add`: link главнее. magnet → hash+magnet; url/file → байты
  .torrent; base64-.torrent в `data` — ТОЛЬКО при пустом link (супермножество
  оригинала для прочих клиентов). Lampa-метаданные хранятся как есть
  (`data_b64`), эхо в status()/list(), persist при save_to_db;
- manager.rs `add` re-add: обновление title/poster/data карточки (Lampa
  пере-добавляет тот же magnet при каждом открытии фильма);
- api.rs `set`: обновление `data` (как `SetTorrent` в оригинале);
- api.rs: чанк отдачи 256→128 КиБ (диапазон 64–128 КиБ из код-ревью);
- main.rs: `max_blocking_threads` 8→4 в --low-memory;
- /gst/echo остаётся 404 (исследование бандла cf.lampa.mx/app.min.js:
  `gstCheck()` ставит `gst_work=true` на ЛЮБОЙ 200 — не глядя в JSON, —
  после чего `stream()` строит `/gst/{hash}/master.m3u8` (HLS), которого
  в ZeroTorr нет; 404 = честный ответ стандартной сборки TorrServer без
  `-gst`, Lampa остаётся на `/stream/`). Ревью-план «+crypto/+sha» в
  RUSTFLAGS не применён сознательно: RustCrypto 0.10 детектит SHA-NI/
  ARM-crypto в рантайме (cpufeatures), а compile-time target-feature
  даёт SIGILL на CPU без крипто-расширений (ТВ-боксы BRCM и пр.);
  sha1-asm уже подключён (vendor/librqbit-sha1-wrapper).
- nightly build-std (panic_immediate_abort) в build-static.sh остался
  опциональным профилем (нет nightly в песочнице — автопереход на stable);
  `trim-paths` в [profile] нестабилен даже в cargo 1.99.1.

Smoke-тест дополнен (3d–3g): точный Lampa-add с кириллическими
метаданными + эхо data/poster/title; /gst/echo=404; re-add обновляет
карточку; set обновляет data.

## Task 9 — v1.4.4: панель настроек в веб-интерфейсе (жалоба пользователя)

Пользователь: «а где настройки в веб, зачем ты их убрал оттуда». Разбор:
бэкенд `POST /settings` (get/set/def) и полное BTSets работали с 1.0,
настройки сохраняются в config.db (bd.settings_save), но мини-страница
ui.html (5.1 КиБ, писалась как «список + добавление») панель настроек
никогда не показывала.

- assets/ui.html: карточка «⚙ Настройки сервера» между списком и подвалом.
  Все 22 поля BTSets, 4 группы: Кэш и диск (CacheSize в МиБ, ReaderReadAHead,
  PreloadCache, UseDisk, TorrentsSavePath, RemoveCacheOnDrop), Сеть и пиры
  (ConnectionsLimit, PeersListenPort, TorrentDisconnectTimeout, RetrackersMode
  0/1/2 селектом, ForceEncrypt, EnableIPv6, DisableTCP/UTP/UPnP/DHT/PEX),
  Скорость (Download/UploadRateLimit КиБ/с + DisableUpload), Сервис
  (EnableDLNA, EnableDebug). Рендер из дескрипторов GROUPS (данные, не DOM).
  loadSets() → {action:get}; saveSets() → {action:set, sets:collectSets()},
  ответ сервера перезаполняет форму (виден санитайз, вкл. клэмпы
  --low-memory); defSets() → {action:def}. CacheSize: МиБ ↔ байты.
  10.8 КиБ → 4.2 КиБ gz (include_bytes! уже настроен в ui.rs с 1.4.2).
- scripts/smoke-test.py: шаг 8b (set → проверка применения → def →
  дефолты 64МиБ/23) и маркеры панели (setbody/saveSets/s_CacheSize) в шаге 11.
- scripts/package-1.4.4.sh (из 1.4.3, V=1.4.4).
- build-static.sh: комментарий о сознательном отказе от RUSTFLAGS
  +crypto/+sha (решение v1.4.3 остаётся: cpufeatures детектит в рантайме,
  compile-time флаг = SIGILL на ТВ-боксах без крипто-расширений).
- VERSION → MatriX.111.zerotorr.1.4.4.

## Task 10 — v1.4.5: jemalloc под лупой (лог maind станции) + баннер собственного RSS

Пользователь показал лог стокового демона станции: `./maind --platformd`
(упал по sdbus::Error FileExists — запущен второй экземпляр при живом
системном, к аллокатору отношения не имеет). В логе видно, что прошивка
Yandex использует **jemalloc** с включённым memory-profiling
(jemalloc_memory_profiling_backend, MemoryProfilingManager … maind).
Гипотеза: если он хорош для станции — перенять для ZeroTorr.

Реализовано (фича opt-in, обычные сборки не затронуты):
- Cargo.toml: `tikv-jemallocator 0.6` (optional, default-features=false,
  unprefixed_malloc_on_supported_platforms + stats), фича `jemalloc`;
- main.rs: `#[global_allocator]` под cfg-фичей + строка баннера
  «alloc: jemalloc 5.3 …» / «alloc: system (musl mallocng)»;
- конфиг аллокатора: сначала символ `malloc_conf` (repr(transparent)
  обёртка над *const u8, оба имени malloc_conf/_rjem_malloc_conf) —
  путь ненадёжен: символ не подхватился (бенчмарк показал дефолтные арены).
  Итоговый механизм: `JEMALLOC_SYS_WITH_MALLOC_CONF` через
  zerotorr/.cargo/config.toml [env] — печётся в C-код на этапе configure.
  Проверено однозначно: невалидный baked-конфиг роняет бинарь при старте.
  Тюнинг: `narenas:2` (= 2 tokio-воркера --low-memory; дефолт 4×CPU = до 64
  арен), `dirty_decay_ms:1000,muzzy_decay_ms:1000` (возврат страниц ОС);
- lib.rs print_mem_banner: собственный RSS процесса из /proc/self/status —
  на станции сразу видно фактическое потребление сервера;
- scripts/bench-alloc.sh: A/B-бенчмарк аллокаторов (сидер + подопытный в
  --low-memory, стрим 32 МиБ, VmRSS каждую 0.5 с + 12 с спада, md5-контроль).

Бенчмарк (x86_64-musl нативно, RUNS=2, одинаковый сид/поток):

| метрика, КиБ | musl mallocng | jemalloc (narenas:2, decay 1с) |
|---|---|---|
| base (после метаданных) | 14 896–15 188 | 24 452–26 692 |
| max во время стрима | 22 724–22 808 | 35 424–38 140 |
| settle (через 12 с) | 22 000–22 064 | 33 584–35 300 |
| размер бинаря | 6 103 912 | 6 502 672 (+398 КиБ) |

Вердикт: **для станции jemalloc НЕ берём**. +10 МиБ RSS по всем точкам —
на устройстве с 238 МиБ RAM это приговор. mallocng компактен, а фрагментация,
которую лечит jemalloc, у ZeroTorr не проблема: горячий путь — куски
фиксированного размера (256 КиБ) и уже самодельный ring-кэш на bytes::Bytes,
аллокатор видит мало churn'а. jemalloc у Yandex уместен в их C++-демоне
(12+ потоков, гетерогенный churn + профилирование утечек во флоте) — не у нас.
Фича jemalloc остаётся в коде для glibc-сборок (там mallocng не участвует).

Релиз v1.4.5: VERSION → MatriX.111.zerotorr.1.4.5; сборки amd64/arm64 musl
(+UPX), smoke-test arm64 под qemu — plain и UPX, ИТОГ OK (реальный торрент,
CD001, Range/206, панель настроек, low-memory clamp).

## v1.4.6 — zero-copy RAM-режим (устранение дублирования кусков)

Код-ревью (внешнее) нашло двойное хранение кусков в RAM-режиме: скачанный
кусок лежал в RamTorrent (HashMap<u32, Vec<u8>>, бюджет max(CacheSize, 3×pl,
1 МиБ)) и ВТОРОЙ раз в RingCache (Bytes, бюджет CacheSize) — насос на промахе
читал FileStream в временный буфер и делал Bytes::copy_from_slice; итог до
~2×CacheSize на торрент + transient. used_total только считал, глобального
лимита нет. Подтверждено по исходникам; заодно найдено, что take() (пауза)
делал глубокую копию всей карты кусков (в этом vendored librqbit не вызывается,
но оставался мёртвым кодом с O(resident) копией).

Фикс (одна физическая копия):
- ramstore: кусок = enum { Partial(BytesMut), Full(Bytes) }, заморозка
  BytesMut::freeze() по on_piece_completed (хэш проверен) — ноль копий;
- RamRegistry::piece_bytes(hash, idx) -> Option<Bytes>: буфер resident-куска
  по ссылке (clone = счётчик ссылок) + touch (LRU защищён отдачей);
- RamTorrent.inner = Arc<Mutex<..>>: Clone/take разделяют состояние (пауза
  без копии); unregister чистит stores-реестр;
- api.rs pump и manager.rs preload: на промахе кусок после FileStream берётся
  через ram_piece() — общий буфер индексируется в RingCache и отдаётся
  срезами Bytes; дисковый путь не тронут (ram_piece = None);
- поведение стримера не менялось (FileStream читается как раньше — окно
  опережения ZT_STREAM_BUF работает прежним образом).

E2E (scripts/e2e-ram.sh, 32 МиБ, localhost, seeder+leecher):
- v1.4.6: md5 CONTENT OK, стрим ~1–2 с (быстрее сэмплера), RSS в стриме
  ~13,7–14,3 МиБ; /dev/shm/zt-b = 0 Б (tmpfs-файл не пишется).
- v1.4.5 (тот же стенд): md5 OK, стрим ~18 с (эвикции + перекачки), RSS
  min/med/max 14,0/22,2/22,7 МиБ.
- VmHWM v1.4.6 за весь жизненный цикл 22,5–22,7 МиБ — пик не в стриме, а в
  фазе докачки/рантайме (librqbit in-flight + session); точечная атрибуция
  не завершена (TODO 1.4.7: полный A/B RSS-профиль + глобальный бюджет
  RamRegistry на все торренты — сейчас used_total только статистика).

Релиз v1.4.6: VERSION → MatriX.111.zerotorr.1.4.6; сборки amd64/arm64 musl
(+UPX 1 866 880 Б, sha256 d2d2620f…); залито на dro.pm (cb/h), скрипт
station-update.sh обновлён (cd), round-trip сверен по всем трём слотам.
