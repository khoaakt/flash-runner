# Build inside the i386 container:  tools/build-guest.sh
CC ?= gcc
CFLAGS = -O2 -g -fno-strict-aliasing -Wall -Wno-unused-function
OUT = ../out
all: $(OUT)/pepper-host
# fwmath.c: floor/ceil/trunc without fnstenv/fldenv (glibc's i386 versions cost four interpreted x87 instructions each and Flash calls them
# millions of times); -rdynamic makes the dynamic linker bind the plugin's calls to these.
$(OUT)/pepper-host: pepper-host.c gen/tables.c gen/stubs.S fwmath.c hc.h ppb_table.h
	mkdir -p $(OUT)
	$(CC) -O2 -fno-builtin -c fwmath.c -o $(OUT)/fwmath.o
	$(CC) $(CFLAGS) -rdynamic -o $@ pepper-host.c gen/tables.c gen/stubs.S $(OUT)/fwmath.o -ldl -lpthread

