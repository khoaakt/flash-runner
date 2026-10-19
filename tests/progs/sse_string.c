// flags: -msse2 -mfpmath=sse
// glibc's ifunc-selected SSE2 string/memory routines against byte-wise reference implementations,
// for every source/destination alignment and every length 0..300, plus page-edge placement.
#define _GNU_SOURCE
#include <stdio.h>
#include <wchar.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <stdint.h>
#include <sys/mman.h>

static unsigned long long H = 14695981039346656037ULL;
static void mix(unsigned long long v) { H = (H ^ v) * 1099511628211ULL; H ^= H >> 31; }
static unsigned long long rs = 0x243f6a8885a308d3ULL;
static unsigned rnd(void) { rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17; return (unsigned)(rs >> 11); }
static int errors;
#define CHECK(c, ...) do { if (!(c)) { if (errors++ < 10) printf("FAIL: " __VA_ARGS__); } } while (0)

static size_t ref_strlen(const char *s) { size_t n = 0; while (s[n]) n++; return n; }
static int sgn(int x) { return (x > 0) - (x < 0); }

static char *a, *b, *c;   // arenas at page edges
#define ARENA 4096

int main(int argc, char **argv) {
    setvbuf(stdout, 0, _IOLBF, 0);
    (void)argc; (void)argv;
    // two pages + guard so buffers can end exactly at a PROT_NONE page
    char *base = mmap(0, 6 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(base + 2 * 4096, 4096, PROT_NONE);
    mprotect(base + 5 * 4096, 4096, PROT_NONE);
    a = base; b = base + 3 * 4096; c = malloc(ARENA * 2);

    // ---- memcpy / memmove / memset / memcmp / memchr / memrchr, all alignments and lengths --------
    for (int sa = 0; sa < 40; sa += (sa < 20 ? 1 : 7)) {
        for (int i = 0; i < 400; i++) a[sa + i] = (char)rnd();
        for (int da = 0; da < 40; da += (da < 20 ? 1 : 3)) {
            for (int n = 0; n <= 300; n += (n < 80 ? 1 : 13)) {
                memset(b, 0x77, 400);
                memcpy(b + da, a + sa, n);
                int ok = 1;
                for (int i = 0; i < n; i++) ok &= b[da + i] == a[sa + i];
                for (int i = 0; i < da; i++) ok &= b[i] == 0x77;
                for (int i = n; i < 64; i++) ok &= b[da + i] == 0x77;
                CHECK(ok, "memcpy sa=%d da=%d n=%d\n", sa, da, n);
                mix(b[da + (n ? n - 1 : 0)]);
            }
        }
    }
    printf("memcpy done\n");
    for (int off = 0; off < 40; off++) {
        for (int n = 0; n <= 300; n += (n < 70 ? 1 : 11)) {
            for (int sh = -20; sh <= 20; sh += 3) {  // overlapping memmove in both directions
                for (int i = 0; i < 700; i++) c[i] = (char)(i * 7 + off);
                char ref[700];
                memcpy(ref, c, 700);
                char tmp[700];
                memcpy(tmp, ref + 100 + off, n);
                memcpy(ref + 100 + off + sh, tmp, n);
                memmove(c + 100 + off + sh, c + 100 + off, n);
                CHECK(!memcmp(c, ref, 700), "memmove off=%d n=%d sh=%d\n", off, n, sh);
            }
        }
    }
    printf("memmove done\n");
    for (int al = 0; al < 40; al++)
        for (int n = 0; n <= 300; n += (n < 70 ? 1 : 9)) {
            memset(a, 0x11, 400);
            memset(a + al, 0xab, n);
            int ok = 1;
            for (int i = 0; i < 400; i++) ok &= (unsigned char)a[i] == ((i >= al && i < al + n) ? 0xab : 0x11);
            CHECK(ok, "memset al=%d n=%d\n", al, n);
        }
    printf("memset done\n");
    for (int al = 0; al < 33; al++)
        for (int n = 0; n <= 130; n++) {
            for (int i = 0; i < 300; i++) a[al + i] = b[al + i] = (char)(i * 13);
            CHECK(memcmp(a + al, b + al, n) == 0, "memcmp eq al=%d n=%d\n", al, n);
            if (n) {
                int p = rnd() % n;
                b[al + p] ^= (char)(1 + rnd() % 255);
                CHECK(sgn(memcmp(a + al, b + al, n)) == sgn((unsigned char)a[al + p] - (unsigned char)b[al + p]), "memcmp ne al=%d n=%d p=%d\n", al, n, p);
            }
        }
    printf("memcmp done\n");
    for (int al = 0; al < 33; al++)
        for (int n = 0; n <= 100; n++)
            for (int p = -1; p < n; p += 5) {
                memset(a, 0x41, 300);
                if (p >= 0) a[al + p] = 'z';
                void *r = memchr(a + al, 'z', n);
                CHECK(r == (p >= 0 ? a + al + p : 0), "memchr al=%d n=%d p=%d\n", al, n, p);
                void *r2 = memrchr(a + al, 'z', n);
                CHECK(r2 == (p >= 0 ? a + al + p : 0), "memrchr al=%d n=%d p=%d\n", al, n, p);
            }
    printf("memchr done\n");

    // ---- string functions ----------------------------------------------------------------------
    for (int al = 0; al < 40; al++)
        for (int len = 0; len <= 200; len += (len < 70 ? 1 : 7)) {
            for (int edge = 0; edge < 2; edge++) {
                // edge==1: string's NUL is the last byte before a PROT_NONE page
                char *s = edge ? base + 2 * 4096 - len - 1 - al % 8 : a + al;
                if (edge && (uintptr_t)s < (uintptr_t)base) continue;
                for (int i = 0; i < len; i++) s[i] = (char)(1 + rnd() % 255);
                s[len] = 0;
                CHECK(strlen(s) == (size_t)len, "strlen al=%d len=%d edge=%d\n", al, len, edge);
                CHECK(strnlen(s, 1000) == (size_t)len, "strnlen\n");
                CHECK(strnlen(s, len / 2) == (size_t)(len / 2), "strnlen2\n");
                if (len) {
                    char ch = s[len / 2];
                    char *e = strchr(s, ch);
                    size_t k = 0; while (s[k] != ch) k++;
                    CHECK(e == s + k, "strchr al=%d len=%d\n", al, len);
                    char *r = strrchr(s, ch);
                    size_t kk = len; while (kk-- > 0 && s[kk] != ch) ;
                    CHECK(r == s + kk, "strrchr al=%d len=%d\n", al, len);
                }
                CHECK(strchr(s, 0) == s + len, "strchr NUL\n");
                CHECK(strchrnul(s, 0) == s + len, "strchrnul\n");
                CHECK(rawmemchr(s, 0) == s + len, "rawmemchr\n");
                char *d = b + (al % 24);
                CHECK(strcpy(d, s) == d && ref_strlen(d) == (size_t)len && !memcmp(d, s, len + 1), "strcpy al=%d len=%d edge=%d\n", al, len, edge);
                CHECK(stpcpy(d, s) == d + len, "stpcpy\n");
                memset(d, 'q', 260);
                char *r = strncpy(d, s, len + 5);
                int ok = r == d && !memcmp(d, s, len + 1);
                for (int i = len + 1; i < len + 5; i++) ok &= d[i] == 0;
                CHECK(ok, "strncpy al=%d len=%d\n", al, len);
                mix(len + al);
            }
        }
    printf("strlen/strchr/strcpy done\n");
    for (int al = 0; al < 24; al += 1)
        for (int len = 0; len <= 100; len += 3)
            for (int dl = 0; dl <= 100; dl += 11) {
                memset(a, 0, 300);
                memset(b, 'x', 300);
                for (int i = 0; i < len; i++) a[al + i] = 'A' + i % 26;
                for (int i = 0; i < dl; i++) b[i] = 'a' + i % 26;
                b[dl] = 0;
                strcat(b, a + al);
                CHECK(ref_strlen(b) == (size_t)(dl + len), "strcat\n");
                memset(b + dl, 0, 100);
                strncat(b, a + al, 7);
                CHECK(ref_strlen(b) == (size_t)(dl + (len < 7 ? len : 7)), "strncat\n");
            }
    printf("strcat done\n");
    for (int al = 0; al < 24; al++)
        for (int bl = 0; bl < 20; bl += 3)
            for (int len = 0; len <= 70; len += 1) {
                for (int i = 0; i < len; i++) a[al + i] = b[bl + i] = (char)('a' + (i * 7) % 26);
                a[al + len] = b[bl + len] = 0;
                CHECK(strcmp(a + al, b + bl) == 0, "strcmp eq\n");
                CHECK(strncmp(a + al, b + bl, len + 3) == 0, "strncmp eq\n");
                CHECK(strcasecmp(a + al, b + bl) == 0, "strcasecmp eq\n");
                if (len) {
                    int p = rnd() % len;
                    char sv = b[bl + p];
                    b[bl + p] = (char)(sv ^ 0x20);   // case change only
                    CHECK(strcasecmp(a + al, b + bl) == 0, "strcasecmp case al=%d bl=%d len=%d\n", al, bl, len);
                    b[bl + p] = (char)(sv + 1);
                    int want = sgn((unsigned char)a[al + p] - (unsigned char)b[bl + p]);
                    CHECK(sgn(strcmp(a + al, b + bl)) == want, "strcmp ne al=%d bl=%d len=%d p=%d\n", al, bl, len, p);
                    CHECK(sgn(strncmp(a + al, b + bl, len)) == want, "strncmp ne\n");
                    b[bl + p] = sv;
                }
            }
    printf("strcmp done\n");
    // strstr / strspn / strcspn / strpbrk / strtok-like searches
    {
        char hay[600];
        for (int t = 0; t < 400; t++) {
            int hl = rnd() % 300;
            for (int i = 0; i < hl; i++) hay[i] = (char)('a' + rnd() % 4);
            hay[hl] = 0;
            int nl = 1 + rnd() % 6;
            char nd[8];
            for (int i = 0; i < nl; i++) nd[i] = (char)('a' + rnd() % 4);
            nd[nl] = 0;
            const char *r = strstr(hay, nd);
            const char *want = 0;
            for (int i = 0; i + nl <= hl; i++) if (!memcmp(hay + i, nd, nl)) { want = hay + i; break; }
            CHECK(r == want, "strstr t=%d\n", t);
            size_t sp = strspn(hay, "ab"), w = 0; while (hay[w] == 'a' || hay[w] == 'b') w++;
            CHECK(sp == w, "strspn\n");
            size_t cs = strcspn(hay, "cd"), w2 = 0; while (hay[w2] && hay[w2] != 'c' && hay[w2] != 'd') w2++;
            CHECK(cs == w2, "strcspn\n");
            const char *pb = strpbrk(hay, "dc"); size_t w3 = 0; while (hay[w3] && hay[w3] != 'd' && hay[w3] != 'c') w3++;
            CHECK(pb == (hay[w3] ? hay + w3 : 0), "strpbrk\n");
            mix(hl + nl + (r ? r - hay : 0));
        }
    }
    printf("strstr/strspn done\n");
    // wide-character routines
    {
        wchar_t w1[200], w2[200];
        for (int al = 0; al < 4; al++)
            for (int len = 0; len < 90; len += 1) {
                for (int i = 0; i < len; i++) w1[al + i] = w2[al + i] = 0x100 + i * 3;
                w1[al + len] = w2[al + len] = 0;
                CHECK(wcslen(w1 + al) == (size_t)len, "wcslen\n");
                CHECK(wcscmp(w1 + al, w2 + al) == 0, "wcscmp\n");
                CHECK(wmemcmp(w1 + al, w2 + al, len) == 0, "wmemcmp\n");
                if (len) CHECK(wcschr(w1 + al, w1[al + len / 2]) == w1 + al + (len / 2), "wcschr\n");
            }
    }
    printf("wide done\n");
    printf("errors=%d hash=%016llx\n", errors, H);
    return errors != 0;
}
