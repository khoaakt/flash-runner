#!/usr/bin/env python3
"""Data files for tests/ext/jank.html (the network-jank probe): tests/out/jank/{big.bin,feed.xml,data.json,big.png}.
   Build the movie with:  haxe -swf tests/swf/jank.swf -swf-version 19 -main Jank -cp tests/swf-src
   Run:  node tools/ext-test.mjs --runtime vendor-private/libpepflashplayer-32.0.0.363.so --page 'tests/ext/jank.html?busy=0' --seconds 75 --fps-log --eval 'window.jank'
   (?busy=300 keeps the embedding page's main thread busy for 300 ms every second: ExternalInterface calls from the movie then wait for the page)"""
import json, os, random, struct, zlib
random.seed(7)
d = os.path.join(os.path.dirname(__file__), '..', 'tests', 'out', 'jank')
os.makedirs(d, exist_ok=True)
open(d + '/big.bin', 'wb').write(os.urandom(8 * 1024 * 1024))
items = ['<item><title>Tin tức số %d: Game cập nhật phiên bản mới</title><link>http://example.com/news/%d</link><description>%s</description><pubDate>Sat, 17 Oct 2026 05:%02d:00 +0700</pubDate><guid>n%d</guid></item>' % (i, i, 'Nội dung bản tin ' + 'lorem ipsum dolor sit amet ' * 14, i % 60, i) for i in range(1400)]
open(d + '/feed.xml', 'w', encoding='utf-8').write('<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><title>feed</title>' + ''.join(items) + '</channel></rss>')
open(d + '/data.json', 'w').write(json.dumps([{"id": i, "name": "item%d" % i, "price": i * 1.5, "tags": ["a", "b", "c"], "nested": {"x": i, "y": [1, 2, 3, 4, 5]}} for i in range(18000)]))
w, h = 1600, 1000
raw = bytearray()
for y in range(h):
    raw.append(0)
    for x in range(w):
        raw += bytes(((x * 255 // w + y * 3) % 256, (y * 255 // h + x) % 256, random.randrange(256)))
def chunk(t, dat):
    c = struct.pack('>I', len(dat)) + t + dat
    return c + struct.pack('>I', zlib.crc32(t + dat) & 0xffffffff)
open(d + '/big.png', 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(bytes(raw), 6)) + chunk(b'IEND', b''))
for f in sorted(os.listdir(d)):
    print(f, os.path.getsize(os.path.join(d, f)))
