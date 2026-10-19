#!/bin/sh
# Serves a generated test stream for tests/ext/rtmp.html: nginx-rtmp in podman, application "stream", stream "test" (colour bars with a counter
# and a 440 Hz tone, made by ffmpeg inside the container; no clip is downloaded or committed).
#   sh tools/rtmp-poc.sh            start (prints the address to pass as ?host=)
#   sh tools/rtmp-poc.sh stop
# The helper refuses private and loopback destinations by default: run the browser with FLASHRUNNER_SOCKETS_CONFIG pointing at a JSON file
# containing {"allowPrivate": true}. Where podman runs in a VM (Windows, macOS) a published port can accept connections and still carry no
# data; use the VM's address instead, which this script prints when it can find it.
set -e
if [ "$1" = stop ]; then podman rm -f rtmp-poc; exit 0; fi
podman rm -f rtmp-poc >/dev/null 2>&1 || true
podman run -d --name rtmp-poc -p 1935:1935 docker.io/alfg/nginx-rtmp >/dev/null
sleep 3
podman exec -d rtmp-poc sh -c 'ffmpeg -loglevel error -re -f lavfi -i testsrc=size=320x240:rate=15 -f lavfi -i sine=frequency=440:sample_rate=44100 -c:v libx264 -preset ultrafast -tune zerolatency -pix_fmt yuv420p -g 30 -c:a aac -ar 44100 -f flv rtmp://127.0.0.1/stream/test'
ip=$(podman machine ssh ip -4 -o addr show eth0 2>/dev/null | sed -n 's/.* inet \([0-9.]*\)\/.*/\1/p' | head -n1)
echo "stream is rtmp://${ip:-127.0.0.1}/stream/test  ->  tests/ext/rtmp.html?host=${ip:-127.0.0.1}"
