#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(void) {
    char path[512]; ssize_t n = readlink("/proc/self/exe", path, sizeof path - 20);
    if (n < 0) { puts("no /proc/self/exe"); return 2; }
    path[n] = 0; char *slash = strrchr(path, '/'); strcpy(slash + 1, "libplug.so");
    void *h = dlopen(path, RTLD_NOW);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    int (*add)(int, int) = dlsym(h, "plug_add");
    int *val = dlsym(h, "plug_value");
    printf("plug_add(2,3)=%d value=%d\n", add(2, 3), *val);
    printf("missing sym: %p\n", dlsym(h, "nope"));
    dlclose(h);
    return 0;
}
