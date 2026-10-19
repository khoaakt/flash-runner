#!/bin/sh
# Builds rootfs/out/rootfs.tar: the minimal i386 glibc userland the guest needs (Debian bookworm).
# Nothing from Adobe/Harman goes in here; the Flash runtime is added separately, at run time.
set -eu
cd "$(dirname "$0")"
mkdir -p out
docker run --rm --platform linux/386 flashrunner-i386-dev sh -c '
  set -e
  L=/usr/lib/i386-linux-gnu
  mkdir -p /stage/usr/lib/i386-linux-gnu /stage/etc /stage/tmp /stage/dev /stage/proc /stage/home/flash
  ln -s usr/lib /stage/lib
  for f in ld-linux.so.2 libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 librt.so.1 libresolv.so.2 libnss_files.so.2 libutil.so.1; do cp -a $L/$f /stage$L/; done
  cp -aL $L/libstdc++.so.6 /stage$L/libstdc++.so.6
  cp -aL $L/libgcc_s.so.1 /stage$L/libgcc_s.so.1
  ln -s i386-linux-gnu/ld-linux.so.2 /stage/usr/lib/ld-linux.so.2
  printf "root:x:0:0:root:/root:/bin/sh\nflash:x:1000:1000:flash:/home/flash:/bin/sh\n" > /stage/etc/passwd
  printf "root:x:0:\nflash:x:1000:\n" > /stage/etc/group
  printf "passwd: files\ngroup: files\nhosts: files\n" > /stage/etc/nsswitch.conf
  printf "127.0.0.1 localhost\n" > /stage/etc/hosts
  chmod 1777 /stage/tmp
  tar cf - -C /stage .
' > out/rootfs.tar
ls -l out/rootfs.tar
