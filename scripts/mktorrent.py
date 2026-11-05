#!/usr/bin/env python3
"""Мини-mktorrent для E2E ZeroTorr: payload.bin + test.torrent + раскладка сиду."""
import hashlib, os, sys

def benc(o):
    if isinstance(o, bool): raise TypeError
    if isinstance(o, int): return b'i%de' % o
    if isinstance(o, bytes): return b'%d:%s' % (len(o), o)
    if isinstance(o, str): return benc(o.encode())
    if isinstance(o, list): return b'l' + b''.join(benc(x) for x in o) + b'e'
    if isinstance(o, dict):
        def kenc(k):
            return k if isinstance(k, bytes) else k.encode()
        return b'd' + b''.join(benc(kenc(k)) + benc(v) for k, v in sorted(o.items())) + b'e'
    raise TypeError(o)

root = sys.argv[1]
size = int(sys.argv[2]) if len(sys.argv) > 2 else 32 * 1024 * 1024
plen = 256 * 1024
name = 'payload.bin'

data = os.urandom(size)
pieces = b''.join(hashlib.sha1(data[i:i+plen]).digest() for i in range(0, size, plen))
info = {b'length': size, b'name': name, b'piece length': plen, b'pieces': pieces}
tor = benc({b'announce': b'http://127.0.0.1:0/ann', b'info': info})
infohash = hashlib.sha1(benc(info)).hexdigest()

open(f'{root}/test.torrent', 'wb').write(tor)
# раскладка сиду: <dir>/cache/<infohash>/payload.bin
seed = f'{root}/a/cache/{infohash}'
os.makedirs(seed, exist_ok=True)
open(f'{seed}/{name}', 'wb').write(data)
open(f'{root}/infohash.txt', 'w').write(infohash)
open(f'{root}/payload.md5', 'w').write(hashlib.md5(data).hexdigest())
print('infohash', infohash, 'torrent', len(tor), 'bytes')
