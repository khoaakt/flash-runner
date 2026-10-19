#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <dirent.h>
#include <errno.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/select.h>
#include <poll.h>

int main(void) {
    mkdir("/tmp/t", 0755);
    FILE *f = fopen("/tmp/t/a.txt", "w");
    for (int i = 0; i < 1000; i++) fprintf(f, "line %d\n", i);
    fclose(f);
    struct stat st; stat("/tmp/t/a.txt", &st);
    printf("size=%ld regular=%d\n", (long)st.st_size, S_ISREG(st.st_mode));
    f = fopen("/tmp/t/a.txt", "r"); char buf[64]; long total = 0; int n = 0;
    while (fgets(buf, sizeof buf, f)) { total += strlen(buf); n++; }
    printf("lines=%d bytes=%ld\n", n, total);
    fseek(f, -9, SEEK_END); fgets(buf, sizeof buf, f); printf("tail=%s", buf);
    fclose(f);
    rename("/tmp/t/a.txt", "/tmp/t/b.txt");
    printf("old exists=%d new exists=%d\n", access("/tmp/t/a.txt", F_OK) == 0, access("/tmp/t/b.txt", F_OK) == 0);
    int fd = open("/tmp/t/b.txt", O_RDONLY);
    char *m = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd, 0);
    printf("mmap head=%.6s\n", m);
    munmap(m, 4096); close(fd);
    int e = open("/nonexistent/x", O_RDONLY); printf("open missing -> %d errno=%d (%s)\n", e, errno, strerror(errno));
    mkdir("/tmp/t/d", 0755);
    for (int i = 0; i < 5; i++) { char p[64]; sprintf(p, "/tmp/t/d/f%d", i); close(open(p, O_CREAT | O_WRONLY, 0644)); }
    DIR *d = opendir("/tmp/t/d"); struct dirent *de; int cnt = 0; long sum = 0;
    while ((de = readdir(d))) { if (de->d_name[0] == 'f') { cnt++; sum += de->d_name[1] - '0'; } }
    closedir(d); printf("dir entries=%d sum=%ld\n", cnt, sum);
    unlink("/tmp/t/b.txt"); printf("after unlink exists=%d\n", access("/tmp/t/b.txt", F_OK) == 0);

    int p[2]; pipe(p);
    write(p[1], "ping", 4);
    fd_set rs; FD_ZERO(&rs); FD_SET(p[0], &rs); struct timeval tv = {1, 0};
    int r = select(p[0] + 1, &rs, 0, 0, &tv);
    char rb[8] = {0}; read(p[0], rb, 4);
    printf("select=%d data=%s\n", r, rb);
    struct pollfd pf = {p[0], POLLIN, 0};
    r = poll(&pf, 1, 10); printf("poll empty=%d\n", r);
    write(p[1], "x", 1); r = poll(&pf, 1, 10); printf("poll ready=%d revents=%d\n", r, pf.revents & POLLIN);
    close(p[1]); read(p[0], rb, 1); r = read(p[0], rb, 1); printf("eof read=%d\n", r);

    int sv[2]; socketpair(1, 1, 0, sv); write(sv[0], "hi", 2); read(sv[1], rb, 2); rb[2] = 0; printf("socketpair=%s\n", rb);
    char cwd[128]; getcwd(cwd, sizeof cwd); printf("cwd=%s\n", cwd);
    FILE *u = fopen("/dev/urandom", "r"); unsigned char rnd[8]; printf("urandom read=%zu\n", fread(rnd, 1, 8, u)); fclose(u);
    FILE *z = fopen("/dev/null", "w"); fputs("x", z); fclose(z);
    return 0;
}
