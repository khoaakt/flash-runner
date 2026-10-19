#include <stdio.h>
int plug_value = 41;
int plug_add(int a, int b) { return a + b + plug_value; }
__attribute__((constructor)) static void plug_init(void) { plug_value++; }
