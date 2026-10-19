Small i386 programs used to compare engines (our emulator, qemu, native) — build with `gcc -O2 -static [-pthread] -m32` inside the i386 container (`flashrunner-i386-dev`):

* `benchx87.c <0-3>` — x87 loops (fmul chain, fldcw/fistp, sin/cos/sqrt, float adds)
* `spintest.c <n>` — n spinning threads + a sleeping main thread (scheduler fairness)
* `x87one.c <op> <modrm>`, `mmxone.c <op> [count]`, `libmsweep.c <fn>` — one-instruction / one-function probes used to find unimplemented instructions
