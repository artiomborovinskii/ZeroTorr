#!/usr/bin/env python3
"""Дымовой тест ZeroTorr: старт, добавление, Lampa-flow (данные-метаданные, /gst/echo),
стрим с Range, ISO-сигнатура, /download speedtest."""
import base64, hashlib, json, os, subprocess, sys, time, urllib.request, urllib.error, signal, shutil

BIN = sys.argv[1]
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 18099
TORRENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ubuntu.torrent")
BASE = f"http://127.0.0.1:{PORT}"
DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testrun-data")


def req(method, path, body=None, headers=None, timeout=30):
    r = urllib.request.Request(BASE + path, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()


BASE2 = ""

def req2(method, path, body=None, headers=None, timeout=30):
    r = urllib.request.Request(BASE2 + path, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()


def infohash_of(torrent_path):
    d = open(torrent_path, "rb").read()

    def skip(buf, i):
        c = buf[i:i + 1]
        if c == b"d" or c == b"l":
            i += 1
            while buf[i:i + 1] != b"e":
                i = skip(buf, i)
            return i + 1
        if c == b"i":
            return buf.index(b"e", i) + 1
        if c.isdigit():
            j = buf.index(b":", i)
            return j + 1 + int(buf[i:j])
        raise ValueError(c)

    i = d.index(b"4:info") + 6
    end = skip(d, i)
    return hashlib.sha1(d[i:end]).hexdigest().upper()


def main():
    shutil.rmtree(DIR, ignore_errors=True)
    os.makedirs(DIR, exist_ok=True)
    srv = subprocess.Popen([BIN, "-p", str(PORT), "-d", DIR],
                           stdout=open(os.path.join(DIR, "server.log"), "wb"),
                           stderr=subprocess.STDOUT)
    global BASE2
    BASE2 = f"http://127.0.0.1:{PORT + 1}"
    ok = True
    try:
        # 1. /echo
        for _ in range(50):
            try:
                st, _, body = req("GET", "/echo")
                if st == 200:
                    break
            except Exception:
                time.sleep(0.3)
        else:
            print("FAIL: сервер не поднялся"); ok = False; return
        print("echo:", body.decode(errors="replace").strip())

        # 2. speedtest /download/300 (Lampa) — читаем 2 МБ из потока
        try:
            r = urllib.request.Request(BASE + "/download/300?vr=123")
            with urllib.request.urlopen(r, timeout=30) as resp:
                dl_status = resp.status
                data = resp.read(2 * 1024 * 1024)
            dl_ok = dl_status == 200 and data == b"\x00" * len(data) and len(data) == 2 * 1024 * 1024
        except Exception as e:
            dl_ok = False
            print("download err:", e)
        print(f"download/300: 2MiB zeros -> {'OK' if dl_ok else 'FAIL'}")
        ok = ok and dl_ok

        # 2b. Range на /download (ServeContent-семантика)
        try:
            r = urllib.request.Request(BASE + "/download/300", headers={"Range": "bytes=1024-2047"})
            with urllib.request.urlopen(r, timeout=30) as resp:
                rd = resp.read()
            range_ok = resp.status == 206 and len(rd) == 1024 and rd == b"\x00" * 1024
        except Exception as e:
            range_ok = False
            print("download range err:", e)
        print(f"download range 206: {'OK' if range_ok else 'FAIL'}")
        ok = ok and range_ok

        # 2c. CORS (Lampa с чужого origin) + Private Network Access
        try:
            st, hdr, _ = req("GET", "/echo", headers={"Origin": "http://lampa.example"})
            cors_ok = st == 200 and hdr.get("access-control-allow-origin") == "*"
            st, hdr, body = req("OPTIONS", "/settings", headers={
                "Origin": "https://lampa.one",
                "Access-Control-Request-Method": "POST",
                "Access-Control-Request-Headers": "content-type, authorization",
                "Access-Control-Request-Private-Network": "true",
            })
            pf_ok = (st == 204
                     and hdr.get("access-control-allow-origin") == "*"
                     and "POST" in hdr.get("access-control-allow-methods", "")
                     and hdr.get("access-control-allow-private-network") == "true"
                     and "authorization" in hdr.get("access-control-allow-headers", "").lower())
        except Exception as e:
            cors_ok = pf_ok = False
            print("cors err:", e)
        print(f"cors origin: {'OK' if cors_ok else 'FAIL'}  preflight+PNA: {'OK' if pf_ok else 'FAIL'}")
        ok = ok and cors_ok and pf_ok

        # 3. добавление: magnet + base64 .torrent (как шлёт UI TorrServer)
        ih = infohash_of(TORRENT)
        b64 = base64.b64encode(open(TORRENT, "rb").read()).decode()
        st, _, body = req("POST", "/torrents", json.dumps(
            {"action": "add",
             "link": f"magnet:?xt=urn:btih:{ih}&dn=ubuntu.iso",
             "data": b64, "title": "ubuntu", "save_to_db": False}).encode())
        try:
            add = json.loads(body)
        except Exception:
            add = {}
        h = add.get("hash", "")
        print(f"add: status={st} stat={add.get('stat')} hash={h[:16]}... name={add.get('name','')[:40]}")
        if not h:
            print("FAIL: нет хэша, ответ:", body[:300]); ok = False; return

        # 3b. add с save_to_db строкой (так шлёт Lampa)
        st, _, body = req("POST", "/torrents", json.dumps(
            {"action": "add", "link": f"magnet:?xt=urn:btih:{ih}",
             "title": "ubuntu-str", "save_to_db": "true"}).encode())
        str_ok = st == 200 and (json.loads(body).get("hash", "").lower() == ih.lower())
        print(f"add save_to_db='true' (строка): {'OK' if str_ok else 'FAIL'}")
        ok = ok and str_ok

        # 3c. /torrents list — bare-массив (Lampa читает result.length)
        st, _, body = req("POST", "/torrents", json.dumps({"action": "list"}).encode())
        try:
            tl = json.loads(body)
        except Exception:
            tl = None
        list_ok = isinstance(tl, list) and any(x.get("hash", "").lower() == ih.lower() for x in tl)
        print(f"torrents list = bare array ({len(tl) if isinstance(tl, list) else 'н/д'}): {'OK' if list_ok else 'FAIL'}")
        ok = ok and list_ok

        # 3d. LAMPА-FLOW (реальный запрос из поля, v1.4.3): add magnet+title+poster+
        #     data={JSON-метаданные фильма}+save_to_db=false+category. До 1.4.3
        #     data декодировался как base64 и add падал с 500 «bad base64 data».
        lampa = {
            "action": "add",
            "link": "magnet:?xt=urn:btih:5436e60e91e860a6c17992bc8b2cbd36fdfd352e&tr=http%3a%2f%2fbt2.t-ru.org%2fann%3fmagnet",
            "title": "[LAMPA] Гарри Поттер и тайная комната / Harry Potter and the Chamber of Secrets [2002, BDRip] Dub",
            "poster": "https://nl.imagetmdb.com/t/p/w500/33Wj3LSyoAqtqkeh7YXcVMvTQzc.jpg?email=",
            "data": json.dumps({"lampa": True, "movie": {"poster_path": "/33Wj3LSyoAqtqkeh7YXcVMvTQzc.jpg",
                           "overview": "Гарри Поттер переходит на второй курс Школы чародейства и волшебства Хогвартс.",
                           "release_date": "2002-11-13", "id": 672,
                           "original_title": "Harry Potter and the Chamber of Secrets",
                           "title": "Гарри Поттер и Тайная комната", "imdb_id": "tt0295297",
                           "source": "tmdb"}}, ensure_ascii=False),
            "save_to_db": False,
            "category": "movie",
        }
        st, _, body = req("POST", "/torrents", json.dumps(lampa, ensure_ascii=False).encode("utf-8"))
        try:
            la = json.loads(body)
        except Exception:
            la = {}
        lh = la.get("hash", "")
        lampa_ok = (st == 200 and lh == "5436e60e91e860a6c17992bc8b2cbd36fdfd352e"
                    and la.get("data", "") == lampa["data"]
                    and la.get("poster", "") == lampa["poster"]
                    and la.get("title", "") == lampa["title"])
        print(f"lampa add (data=метаданные): status={st} hash={lh[:16]}… data_echo={'yes' if la.get('data') else 'no'} -> {'OK' if lampa_ok else 'FAIL'}")
        if not lampa_ok and st != 200:
            print("   ответ:", body[:200])
        ok = ok and lampa_ok

        # 3e. /gst/echo -> 404: так и задумано. В TorrServer без -gst этого роута нет;
        #     Lampa по любому 200 ставит gst_work=true и строит /gst/{hash}/master.m3u8
        #     (HLS-транскодинг), которого в ZeroTorr нет. 404 = транскодинг выключен.
        st, _, _ = req("GET", "/gst/echo")
        gst_ok = st == 404
        print(f"gst/echo -> {st} (404 = Lampa не включит HLS): {'OK' if gst_ok else 'FAIL'}")
        ok = ok and gst_ok

        # 3f. re-add того же magnet с другими метаданными — карточка обновляется
        lampa2 = dict(lampa, poster="https://example.com/p2.jpg",
                      data=json.dumps({"lampa": True, "movie": {"id": 672, "v": 2}}, ensure_ascii=False))
        st, _, body = req("POST", "/torrents", json.dumps(lampa2, ensure_ascii=False).encode("utf-8"))
        try:
            la2 = json.loads(body)
        except Exception:
            la2 = {}
        readd_ok = st == 200 and la2.get("data", "") == lampa2["data"] and la2.get("poster", "") == lampa2["poster"]
        print(f"lampa re-add (обновление карточки): {'OK' if readd_ok else 'FAIL'}")
        ok = ok and readd_ok

        # 3g. set обновляет data (SetTorrent в MatriX.111)
        d3 = json.dumps({"lampa": True, "movie": {"id": 672, "v": 3}}, ensure_ascii=False)
        st, _, body = req("POST", "/torrents", json.dumps(
            {"action": "set", "hash": lh, "data": d3}, ensure_ascii=False).encode("utf-8"))
        st, _, body = req("POST", "/torrents", json.dumps({"action": "get", "hash": lh}).encode())
        try:
            g3 = json.loads(body)
        except Exception:
            g3 = {}
        set_ok = st == 200 and g3.get("data", "") == d3
        print(f"set data (SetTorrent): {'OK' if set_ok else 'FAIL'}")
        ok = ok and set_ok

        # 4. ожидание метаданных/пиров
        deadline, info = time.time() + 180, {}
        while time.time() < deadline:
            try:
                st, _, body = req("POST", "/torrents", json.dumps({"action": "get", "hash": h}).encode())
                info = json.loads(body)
            except Exception:
                pass
            if info.get("torrent_size", 0) > 0 and info.get("stat", 0) >= 3:
                break
            time.sleep(2)
        print(f"torrent: stat={info.get('stat')} stat_string={info.get('stat_string')} "
              f"size={info.get('torrent_size')} peers={info.get('total_peers')} "
              f"seeders={info.get('connected_seeders')}")
        if info.get("torrent_size", 0) <= 0:
            print("FAIL: нет метаданных"); ok = False; return

        # 5. стрим: Range 0-1048575, ISO-сигнатура CD001 на 0x8001
        data = b""
        st = 0
        for attempt in range(8):
            try:
                st, hdr, data = req("GET", f"/stream/ubuntu.iso?link={h}&index=0&play",
                                    headers={"Range": "bytes=0-1048575"}, timeout=120)
                if st == 206 and len(data) == 1048576:
                    break
            except Exception:
                pass
            time.sleep(10)
        sig_ok = len(data) == 1048576 and data[0x8001:0x8006] == b"CD001"
        print(f"stream: status={st} len={len(data)} CD001@0x8001={data[0x8001:0x8006]} -> {'OK' if sig_ok else 'FAIL'}")
        ok = ok and sig_ok and st == 206

        # 6. перемотка в середину
        st, _, data2 = req("GET", f"/stream/ubuntu.iso?link={h}&index=0&play",
                           headers={"Range": "bytes=104857600-104960000"}, timeout=120)
        seek_ok = st == 206 and len(data2) == 102401
        print(f"seek: status={st} len={len(data2)} -> {'OK' if seek_ok else 'FAIL'}")
        ok = ok and seek_ok

        # 7. viewed после play + set с timecode + фильтр по хэшу
        st, _, body = req("POST", "/viewed", json.dumps({"action": "list"}).encode())
        v = json.loads(body)
        viewed_ok = isinstance(v, list) and any(x.get("hash") == h for x in v)
        # set с timecode (как viewedSet в Lampa)
        st, _, _ = req("POST", "/viewed", json.dumps(
            {"action": "set", "hash": h, "file_index": 1, "timecode": 42.5}).encode())
        st, _, body = req("POST", "/viewed", json.dumps({"action": "list", "hash": h}).encode())
        vl = json.loads(body)
        tc_ok = (isinstance(vl, list)
                 and all(x.get("hash") == h for x in vl)
                 and any(x.get("file_index") == 1 and abs(x.get("timecode", 0) - 42.5) < 0.01 for x in vl))
        # чужой хэш -> пустой массив (Lampa доверяет серверной фильтрации!)
        st, _, body = req("POST", "/viewed", json.dumps({"action": "list", "hash": "0" * 40}).encode())
        v3 = json.loads(body)
        filt_ok = v3 == []
        print(f"viewed: {'OK' if viewed_ok else 'FAIL'}  timecode+фильтр: {'OK' if tc_ok and filt_ok else 'FAIL'}")
        ok = ok and viewed_ok and tc_ok and filt_ok

        # 8. settings get
        st, _, body = req("POST", "/settings", json.dumps({"action": "get"}).encode())
        sets = json.loads(body)
        settings_ok = st == 200 and sets.get("CacheSize") == 67108864 and sets.get("ReaderReadAHead") == 95
        print(f"settings: CacheSize={sets.get('CacheSize')} ReaderReadAHead={sets.get('ReaderReadAHead')} ConnectionsLimit={sets.get('ConnectionsLimit')} -> {'OK' if settings_ok else 'FAIL'}")
        ok = ok and settings_ok

        # 8b. settings set/def roundtrip (панель настроек UI, v1.4.4): set -> проверка
        #     применения -> def -> значения по умолчанию
        st, _, body = req("POST", "/settings", json.dumps(
            {"action": "set", "sets": {"CacheSize": 32 * 1024 * 1024, "ConnectionsLimit": 40,
                                        "RetrackersMode": 2, "DisableUpload": True}}).encode())
        s2 = json.loads(body)
        sets_set_ok = (st == 200 and s2.get("CacheSize") == 32 * 1024 * 1024
                       and s2.get("ConnectionsLimit") == 40 and s2.get("RetrackersMode") == 2
                       and s2.get("DisableUpload") is True)
        st, _, body = req("POST", "/settings", json.dumps({"action": "def"}).encode())
        s3 = json.loads(body)
        sets_def_ok = st == 200 and s3.get("CacheSize") == 64 * 1024 * 1024 and s3.get("ConnectionsLimit") == 23
        print(f"settings set/def (панель UI): {'OK' if sets_set_ok and sets_def_ok else 'FAIL'}")
        ok = ok and sets_set_ok and sets_def_ok

        # 9. playlist
        st, _, body = req("GET", f"/playlist?hash={h}")
        pl_ok = st == 200 and b"#EXTM3U" in body
        print(f"playlist: {'OK' if pl_ok else 'FAIL'}")
        ok = ok and pl_ok

        # 10. cache state
        st, _, body = req("POST", "/cache", json.dumps({"action": "get", "hash": h}).encode())
        c = json.loads(body)
        cache_ok = st == 200 and c.get("PiecesLength", 0) > 0 and c.get("Filled", 0) > 0
        print(f"cache: PiecesLength={c.get('PiecesLength')} Filled={c.get('Filled')} -> {'OK' if cache_ok else 'FAIL'}")
        ok = ok and cache_ok

        # 11. UI — с 1.4.2 отдаётся готовым gzip (Content-Encoding), как браузеру
        st, hdr, body = req("GET", "/", headers={"Accept-Encoding": "gzip"})
        if hdr.get("content-encoding") == "gzip":
            import gzip as _gz
            body = _gz.decompress(body)
        ui_ok = (st == 200 and hdr.get("content-encoding") == "gzip" and b"ZeroTorr" in body
                 and b"setbody" in body and b"saveSets" in body  # панель настроек, v1.4.4
                 and b'"CacheSize"' in body and b'"RetrackersMode"' in body)  # дескрипторы полей
        print(f"ui: {'OK' if ui_ok else 'FAIL'} (gzip: {hdr.get('content-encoding')}, настройки: "
              f"{'есть' if b'saveSets' in body and b'\"CacheSize\"' in body else 'нет'})")
        ok = ok and ui_ok

        # 12. shutdown
        req("GET", "/shutdown")
        try:
            srv.wait(timeout=10)
            print(f"shutdown: OK (exit={srv.returncode})")
        except subprocess.TimeoutExpired:
            srv.kill(); print("shutdown: FAIL (не завершился)"); ok = False

        # 13. --low-memory: настройки из API не поднимаются сверх лимита
        dir2 = DIR + "-lowmem"
        shutil.rmtree(dir2, ignore_errors=True)
        os.makedirs(dir2, exist_ok=True)
        srv2 = subprocess.Popen([BIN, "-p", str(PORT + 1), "-d", dir2, "--low-memory"],
                                stdout=open(os.path.join(dir2, "server.log"), "wb"),
                                stderr=subprocess.STDOUT)
        try:
            for _ in range(50):
                try:
                    st, _, _ = req2("GET", "/echo")
                    if st == 200:
                        break
                except Exception:
                    time.sleep(0.3)
            st, _, body = req2("POST", "/settings", json.dumps(
                {"action": "set", "sets": {"CacheSize": 536870912, "ConnectionsLimit": 50,
                                            "ReaderReadAHead": 95, "PreloadCache": 50,
                                            "TorrentDisconnectTimeout": 30}}).encode())
            sets2 = json.loads(body)
            lm_ok = (st == 200
                     and sets2.get("CacheSize", 0) <= 8 * 1024 * 1024
                     and sets2.get("ConnectionsLimit") == 8
                     and sets2.get("TorrentDisconnectTimeout") == 15
                     and sets2.get("ReaderReadAHead") == 50
                     and sets2.get("PreloadCache") == 10)
            print(f"low-memory clamp: CacheSize={sets2.get('CacheSize')} peers={sets2.get('ConnectionsLimit')} "
                  f"RA={sets2.get('ReaderReadAHead')} -> {'OK' if lm_ok else 'FAIL'}")
            ok = ok and lm_ok
            req2("GET", "/shutdown")
            srv2.wait(timeout=10)
        except Exception as e:
            print("low-memory err:", e); ok = False
        finally:
            if srv2.poll() is None:
                srv2.send_signal(signal.SIGKILL)
    finally:
        if srv.poll() is None:
            srv.send_signal(signal.SIGKILL)
    print("\nИТОГ:", "OK" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
