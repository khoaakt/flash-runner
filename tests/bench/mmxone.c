// one MMX/SSE2-int op per run: mmxone <op>  -> prints "op result"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <stdint.h>
#include <mmintrin.h>
#include <emmintrin.h>
typedef uint64_t u64; typedef uint32_t u32; typedef uint16_t u16;
#define BIN(X) X(paddb) X(paddw) X(paddd) X(paddq) X(psubb) X(psubw) X(psubd) X(psubq) X(paddsb) X(paddsw) X(paddusb) X(paddusw) X(psubsb) X(psubsw) X(psubusb) X(psubusw) \
  X(pmullw) X(pmulhw) X(pmulhuw) X(pmuludq) X(pmaddwd) X(pavgb) X(pavgw) X(pminub) X(pmaxub) X(pminsw) X(pmaxsw) X(psadbw) \
  X(pcmpeqb) X(pcmpeqw) X(pcmpeqd) X(pcmpgtb) X(pcmpgtw) X(pcmpgtd) X(pand) X(pandn) X(por) X(pxor) X(packsswb) X(packssdw) X(packuswb) \
  X(punpcklbw) X(punpcklwd) X(punpckldq) X(punpckhbw) X(punpckhwd) X(punpckhdq) X(psllw) X(pslld) X(psllq) X(psrlw) X(psrld) X(psrlq) X(psraw) X(psrad)
#define DEF(op) static u64 op##_rr(u64 a, u64 b) { __m64 x = (__m64)a, y = (__m64)b; asm volatile(#op " %1, %0" : "+y"(x) : "y"(y)); u64 r = (u64)x; _mm_empty(); return r; } \
  static u64 op##_rm(u64 a, const u64 *p) { __m64 x = (__m64)a; asm volatile(#op " %1, %0" : "+y"(x) : "m"(*p)); u64 r = (u64)x; _mm_empty(); return r; }
BIN(DEF)
#define IMM(op) static u64 op##_i(u64 a) { __m64 x = (__m64)a; asm volatile(#op " $3, %0" : "+y"(x)); u64 r = (u64)x; _mm_empty(); return r; }
IMM(psllw) IMM(pslld) IMM(psllq) IMM(psrlw) IMM(psrld) IMM(psrlq) IMM(psraw) IMM(psrad)
static u64 pshufw_i(u64 a) { __m64 x = (__m64)a, r; asm volatile("pshufw $27, %1, %0" : "=y"(r) : "y"(x)); u64 o = (u64)r; _mm_empty(); return o; }
static u32 pextrw_i(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("pextrw $2, %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
static u64 pinsrw_i(u64 a) { __m64 x = (__m64)a; asm volatile("pinsrw $1, %1, %0" : "+y"(x) : "r"(0xBEEF)); u64 r = (u64)x; _mm_empty(); return r; }
static u32 pmovmskb_i(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("pmovmskb %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
static u64 movd_in(u32 v) { __m64 r; asm volatile("movd %1, %0" : "=y"(r) : "r"(v)); u64 o = (u64)r; _mm_empty(); return o; }
static u32 movd_out(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("movd %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
static u64 movq_ld(const u64 *p) { __m64 r; asm volatile("movq %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }
static u64 movq_st(u64 a) { u64 m = 0; __m64 x = (__m64)a; asm volatile("movq %1, %0" : "=m"(m) : "y"(x) : "memory"); _mm_empty(); return m; }
static u64 movntq_i(u64 a) { u64 m = 0; __m64 x = (__m64)a; asm volatile("movntq %1, %0" : "=m"(m) : "y"(x) : "memory"); _mm_empty(); return m; }
static u64 maskmovq_i(u64 d, u64 m) { u64 buf = 0x1111111111111111ULL; __m64 x = (__m64)d, y = (__m64)m; asm volatile("maskmovq %1, %0" :: "y"(x), "y"(y), "D"(&buf) : "memory"); _mm_empty(); return buf; }
static u64 movq2dq_i(u64 a) { __m64 x = (__m64)a; __m128i r; asm volatile("movq2dq %1, %0" : "=x"(r) : "y"(x)); _mm_empty(); u64 o[2]; memcpy(o, &r, 16); return o[0] ^ (o[1] * 3); }
static u64 movdq2q_i(u64 a) { __m128i v = _mm_set_epi64x(0x1122334455667788LL, (long long)a); __m64 r; asm volatile("movdq2q %1, %0" : "=y"(r) : "x"(v)); u64 o = (u64)r; _mm_empty(); return o; }
static u64 emms_i(void) { asm volatile("emms"); return 1; }
static u64 mmx_x87_mix(u64 a) { double d = 1.5; u64 r; __m64 x = (__m64)a; asm volatile("fld1; paddb %1,%1; emms; fstp %%st(0)" : "+y"(x) :: "st"); r = (u64)x; return r + (u64)d; }
int main(int argc, char **argv) {
  const u64 A = 0x8001FF7F00FE0180ULL; u64 B = 0x7FFF0102FF80807FULL; if (argc > 2) B = strtoull(argv[2], 0, 16); u64 r = 0; u32 r32 = 0; const char *op = argc > 1 ? argv[1] : "";
  char name[48]; strncpy(name, op, 40); name[40] = 0; int len = (int)strlen(name); int rm = 0;
  if (len > 3 && !strcmp(name + len - 3, "_rm")) { rm = 1; name[len - 3] = 0; } else if (len > 3 && !strcmp(name + len - 3, "_rr")) name[len - 3] = 0;
  u64 bb = B;
#define CH(o) if (!strcmp(name, #o)) { r = rm ? o##_rm(A, &bb) : o##_rr(A, B); printf("%s %016llx\n", op, (unsigned long long)r); return 0; }
  BIN(CH)
#define CI(o) if (!strcmp(op, #o "_i")) { r = o##_i(A); printf("%s %016llx\n", op, (unsigned long long)r); return 0; }
  if (!strcmp(op, "movq_st")) { printf("%s %016llx\n", op, (unsigned long long)movq_st(A)); return 0; }
  CI(psllw) CI(pslld) CI(psllq) CI(psrlw) CI(psrld) CI(psrlq) CI(psraw) CI(psrad) CI(pshufw) CI(pinsrw) CI(movq2dq) CI(movdq2q) CI(movntq)
  if (!strcmp(op, "pextrw")) { r32 = pextrw_i(A); printf("%s %08x\n", op, r32); return 0; }
  if (!strcmp(op, "pmovmskb")) { r32 = pmovmskb_i(A); printf("%s %08x\n", op, r32); return 0; }
  if (!strcmp(op, "movd_in")) { printf("%s %016llx\n", op, (unsigned long long)movd_in(0xCAFEBABE)); return 0; }
  if (!strcmp(op, "movd_out")) { printf("%s %08x\n", op, movd_out(A)); return 0; }
  if (!strcmp(op, "movq_ld")) { printf("%s %016llx\n", op, (unsigned long long)movq_ld(&bb)); return 0; }
  if (!strcmp(op, "maskmovq")) { printf("%s %016llx\n", op, (unsigned long long)maskmovq_i(A, 0x80FF00FF8000FF00ULL)); return 0; }
  if (!strcmp(op, "emms")) { printf("%s %llu\n", op, (unsigned long long)emms_i()); return 0; }
  if (!strcmp(op, "mixx87")) { printf("%s %016llx\n", op, (unsigned long long)mmx_x87_mix(A)); return 0; }
  printf("unknown op %s\n", op); return 2;
}
