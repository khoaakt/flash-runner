// flags: -msse2 -mfpmath=sse
// SSE/SSE2 kernel (float + integer lanes) mixed with scalar loop control: stresses the JIT's interpreter fallback.
#include <emmintrin.h>
#include <stdio.h>
#include <stdint.h>
#define N 4096
static float a[N] __attribute__((aligned(16))), b[N] __attribute__((aligned(16))), c[N] __attribute__((aligned(16)));
static int16_t s1[N] __attribute__((aligned(16))), s2[N] __attribute__((aligned(16)));
int main(void) {
    for (int i = 0; i < N; i++) { a[i] = (float)(i % 17) * 0.5f; b[i] = (float)(i % 5) + 1.0f; s1[i] = (int16_t)(i * 37); s2[i] = (int16_t)(i * 11); }
    __m128i acc = _mm_setzero_si128();
    for (int r = 0; r < 3000; r++) {
        for (int i = 0; i < N; i += 4) {
            __m128 x = _mm_load_ps(a + i), y = _mm_load_ps(b + i);
            _mm_store_ps(c + i, _mm_add_ps(_mm_mul_ps(x, y), x));
        }
        for (int i = 0; i < N; i += 8) {
            __m128i p = _mm_load_si128((const __m128i *)(s1 + i)), q = _mm_load_si128((const __m128i *)(s2 + i));
            acc = _mm_add_epi16(acc, _mm_mullo_epi16(p, q));
        }
        a[r & 63] += 1.0f;
    }
    uint32_t t[4]; _mm_storeu_si128((__m128i *)t, acc);
    double sum = 0; for (int i = 0; i < N; i++) sum += c[i];
    printf("sum=%.1f acc=%08x%08x%08x%08x\n", sum, t[0], t[1], t[2], t[3]);
    return 0;
}
