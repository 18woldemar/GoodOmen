/* peek.c -- read the original engine's own state while it runs.
 *
 * LD_PRELOAD this into Wine.  Reading from outside is not an option here:
 * Wine reparents every one of its processes to init, so with
 * kernel.yama.ptrace_scope=1 nothing outside is an ancestor and
 * process_vm_readv returns EPERM.  Measured, not assumed.  From inside the
 * process there is no permission question at all.
 *
 * Three things about this that cost a run each to learn:
 *
 * 1. Configured by a file, not the environment.  Wine rebuilds the
 *    environment for the Windows process it starts, so exported PEEK_*
 *    variables reach the helper processes and never the game.  LD_PRELOAD
 *    itself survives, being the loader's own.  tools/peek.sh bakes the
 *    config path in at build time.
 *
 * 2. Memory is read through /proc/self/mem, not by dereferencing the
 *    address.  A writable mapping in a Wine process is not necessarily a
 *    page you may touch -- wineserver's shared mappings and the reserved
 *    low ranges are in the list too -- and a direct read kills the game
 *    outright, which looks exactly like "LD_PRELOAD did not work".  pread
 *    returns an error instead of a signal.
 *
 * 3. A worker thread, not a hook on the frame.  Hooking eglSwapBuffers
 *    would put every sample on a frame boundary for free, and that is what
 *    the Linux capture tools do -- but Wine reaches EGL through dlsym on
 *    its own libEGL handle, which plain symbol interposition does not
 *    intercept, so the hook writes nothing and says nothing.
 *    ponytail: samples are not frame-aligned.  If aligning them ever
 *    matters, wrap dlsym and hook eglSwapBuffers; that is the whole
 *    upgrade.
 *
 * The game's image is not relocated: mdk2Main.exe was linked by MSVC 6,
 * which predates /DYNAMICBASE, and Wine randomises neither the exe nor the
 * heap nor the stack.  Two runs put it at 0x400000 both times, so an
 * address found once is worth writing down.
 *
 * Config file, one key=value a line:
 *   out=/path/to/log     where to write; the pid is appended
 *   find=271,-69         hunt for two consecutive floats (the scan)
 *   ptr=0x1a2b3c4        hunt instead for a 4-byte pointer of this value
 *   eps=0.05             how near a float must be to count
 *   watch=0x1a2b3c4      skip the scan, log this address (repeatable)
 *   lo=0x10000           the window to scan, defaulting to the 32-bit
 *   hi=0x100000000       address space, because that is all a 32-bit
 *                        program has.  Without the window the first thing
 *                        the scan found was a matching pair of floats up
 *                        at 0x7f4f..., in the 64-bit host's own heap: it
 *                        never moved and was never the player.
 *   len=32               bytes to log per candidate per sample
 *   hz=200               samples a second (0 = scan once, then stop)
 *   every=0.5            seconds between scans while still hunting.  It
 *                        has to be well under a second: with a demo
 *                        playing, the player holds the spawn position for
 *                        only a moment before walking off it.
 *
 * Build with tools/peek.sh, which writes libpeek.so -- not peek.so,
 * which would shadow peek.py on import and break tools/check.py.  The analysis lives in tools/peek.py, which is
 * where the self-test is: this file is deliberately dumb.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <math.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>

#define MAX_CAND 256
#define CHUNK    (1 << 20)

static FILE *out;
static int mem = -1;                 /* /proc/self/mem */
static uintptr_t cand[MAX_CAND];
static int ncand;
static float want[2];
static float eps = 0.05f;
static unsigned pattern;
static int use_ptr, loglen = 32, hz = 200;
static uintptr_t lo = 0x10000, hi = 0x100000000ul;
static double every = 0.5;

struct region { uintptr_t lo, hi; };

/* Snapshot /proc/self/maps whole: walking the file while scanning would
 * read something the scan itself can change. */
static int regions(struct region *r, int max, int *is_game)
{
    FILE *f = fopen("/proc/self/maps", "r");
    char line[512];
    int n = 0;
    if (!f) return 0;
    *is_game = 0;
    while (fgets(line, sizeof line, f)) {
        uintptr_t lo, hi;
        char perms[8], path[320];
        path[0] = 0;
        if (sscanf(line, "%lx-%lx %7s %*s %*s %*s %319[^\n]",
                   &lo, &hi, perms, path) < 3) continue;
        if (strstr(path, "mdk2Main.exe")) *is_game = 1;
        if (n >= max || perms[1] != 'w') continue;
        if (strncmp(path, "/dev/", 5) == 0) continue;   /* never touch devices */
        r[n].lo = lo; r[n].hi = hi; n++;
    }
    fclose(f);
    return n;
}

/* Read from our own address space without risking a signal. */
static ssize_t grab(void *dst, uintptr_t addr, size_t len)
{
    return pread(mem, dst, len, (off_t)addr);
}

static void scan(struct region *r, int n)
{
    static unsigned char buf[CHUNK + 8];
    int i;
    for (i = 0; i < n && ncand < MAX_CAND; i++) {
        uintptr_t a = r[i].lo < lo ? lo : r[i].lo;
        uintptr_t end = r[i].hi > hi ? hi : r[i].hi;
        while (a + 8 <= end && ncand < MAX_CAND) {
            size_t len = end - a;
            size_t j;
            if (len > CHUNK) len = CHUNK;
            if (grab(buf, a, len) != (ssize_t)len) break;   /* not ours to read */
            for (j = 0; j + 8 <= len; j += 4) {
                if (use_ptr) {
                    unsigned v;
                    memcpy(&v, buf + j, 4);
                    if (v == pattern && ncand < MAX_CAND) cand[ncand++] = a + j;
                } else {
                    float p[2];
                    memcpy(p, buf + j, 8);
                    if (fabsf(p[0] - want[0]) <= eps &&
                        fabsf(p[1] - want[1]) <= eps && ncand < MAX_CAND)
                        cand[ncand++] = a + j;
                }
            }
            a += len;
        }
    }
}

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

static void *worker(void *unused)
{
    static struct region r[8192];
    static float v[128];
    double t0 = now();
    int n, game = 0, pass = 0;
    (void)unused;

    /* The exe is mapped early, but the level -- and the value -- are not.
     * Rescan until something matches, then never again. */
    while (!ncand) {
        n = regions(r, 8192, &game);
        if (game) { scan(r, n); pass++; }
        if (!ncand) {
            fprintf(out, "# pass %d: %d regions%s\n", pass, n,
                    game ? "" : ", exe not mapped yet");
            fflush(out);
            usleep((useconds_t)(every * 1e6));
        }
    }
    fprintf(out, "# found %d after %.1fs\n", ncand, now() - t0);
    for (n = 0; n < ncand; n++) fprintf(out, "# cand %lx\n", (unsigned long)cand[n]);
    fflush(out);

    while (hz > 0) {
        int i, j, w = loglen / 4;
        if (w > 128) w = 128;
        for (i = 0; i < ncand; i++) {
            if (grab(v, cand[i], w * 4) != w * 4) continue;
            fprintf(out, "%.4f %lx", now() - t0, (unsigned long)cand[i]);
            for (j = 0; j < w; j++) fprintf(out, " %.4f", v[j]);
            fputc('\n', out);
        }
        fflush(out);
        usleep(1000000 / hz);
    }
    return NULL;
}

#ifndef PEEK_CONF
#define PEEK_CONF "peek.conf"
#endif

__attribute__((constructor)) static void peek_start(void)
{
    FILE *cf = fopen(PEEK_CONF, "r");
    char line[512], path[600], outpath[512] = "";
    pthread_t th;

    if (!cf) return;
    while (fgets(line, sizeof line, cf)) {
        char *v = strchr(line, '=');
        if (line[0] == '#' || !v) continue;
        *v++ = 0;
        v[strcspn(v, "\r\n")] = 0;
        if (!strcmp(line, "out")) snprintf(outpath, sizeof outpath, "%s", v);
        else if (!strcmp(line, "eps")) eps = strtof(v, NULL);
        else if (!strcmp(line, "len")) loglen = atoi(v);
        else if (!strcmp(line, "hz")) hz = atoi(v);
        else if (!strcmp(line, "ptr")) { pattern = (unsigned)strtoul(v, NULL, 0); use_ptr = 1; }
        else if (!strcmp(line, "watch") && ncand < MAX_CAND)
            cand[ncand++] = (uintptr_t)strtoul(v, NULL, 0);
        else if (!strcmp(line, "every")) every = strtod(v, NULL);
        else if (!strcmp(line, "lo")) lo = (uintptr_t)strtoull(v, NULL, 0);
        else if (!strcmp(line, "hi")) hi = (uintptr_t)strtoull(v, NULL, 0);
        else if (!strcmp(line, "find")) sscanf(v, "%f,%f", &want[0], &want[1]);
    }
    fclose(cf);
    if (!outpath[0]) return;
    snprintf(path, sizeof path, "%s.%d", outpath, (int)getpid());
    if (!(out = fopen(path, "w"))) return;
    if ((mem = open("/proc/self/mem", O_RDONLY)) < 0) return;
    fprintf(out, "# peek pid %d eps=%g len=%d hz=%d %s\n", (int)getpid(),
            eps, loglen, hz, use_ptr ? "ptr" : ncand ? "watch" : "find");
    fflush(out);
    pthread_create(&th, NULL, worker, NULL);
}
