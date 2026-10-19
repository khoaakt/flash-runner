// x87one <op-hex> <modrm-hex>: execute "op modrm" (memory operand [eax] when mod != 3) on a tiny FPU state and print the result.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>
int main(int argc, char **argv) {
  if (argc < 3) return 2;
  unsigned op = strtoul(argv[1], 0, 16), mrm = strtoul(argv[2], 0, 16);
  static unsigned char buf[32] __attribute__((aligned(16)));
  double d = 3.5; int32_t i32 = 7; int16_t i16 = 5; int64_t i64 = 9; float f = 2.25f;
  memset(buf, 0, sizeof buf);
  // operand data valid for every size: 8 bytes of an int64 5 ... ; for float forms the bytes are a float 2.25 in the low 4 bytes
  memcpy(buf, &f, 4); memcpy(buf + 4, &i32, 4);
  unsigned char *code = mmap(0, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  // code: push regs; mov eax,buf; fninit; fld1 (st1 = 1.0); fldpi? use fld qword [d]; <op modrm>; fstp qword [res]; ret
  static double res[4];
  unsigned char *p = code;
  *p++ = 0xb8; uint32_t a = (uint32_t)(uintptr_t)buf; memcpy(p, &a, 4); p += 4;     // mov eax, buf
  *p++ = 0xdb; *p++ = 0xe3;                                                      // fninit
  for (int k = 0; k < 3; k++) { *p++ = 0xd9; *p++ = 0xe8; }                      // fld1 x3 (deep enough that any popping op leaves a valid stack)
  *p++ = 0xdd; *p++ = 0x05; { uint32_t da = (uint32_t)(uintptr_t)&d; memcpy(p, &da, 4); p += 4; }   // fld qword [d]
  *p++ = (unsigned char)op; *p++ = (unsigned char)mrm;
  *p++ = 0xdd; *p++ = 0x1d; { uint32_t ra = (uint32_t)(uintptr_t)&res[0]; memcpy(p, &ra, 4); p += 4; }  // fstp qword [res]
  *p++ = 0xdb; *p++ = 0xe3;                                                      // fninit (clear the stack without popping)
  *p++ = 0xc3;
  ((void (*)(void))code)();
  printf("x87one %02x %02x ok res=%g\n", op, mrm, res[0]);
  return 0;
}
