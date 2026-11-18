/*
 * glaeda-cdu: clone-aware disk usage for glaeda-disk --root-ledger. Report only; it never writes.
 *
 * du counts every APFS clone in full, so a volume of cloned DerivedData and seeds can "use" several times the
 * disk (one host: 2.3 TiB on a 460 GiB disk). On macOS this walker asks each file that may share blocks
 * (getattrlist ATTR_CMNEXT_EXT_FLAGS & EF_MAY_SHARE_BLOCKS) for its physical extents (fcntl F_LOG2PHYS_EXT) and
 * marks them in a sparse block bitmap, so every block is counted once, to the first path that reaches it.
 * Files that share nothing count their allocated blocks; hard links count once. Elsewhere (the Linux build is
 * only a syntax check and a hard-link-aware du) every file counts its allocated blocks.
 *
 *     glaeda-cdu [-d DEPTH] [-t SECONDS] [-s DIR_SECONDS] [-x PATH]... [-v] ROOT
 *
 * TCC-protected folders (TCC_SKIP below, under any Library) are not entered: as root, reading one can block in
 * the kernel waiting on tccd (one host hung in a user's Library/News). -x skips more paths (the ledger passes the
 * ones an earlier walk stalled in). A watchdog thread bounds every directory read and file map to DIR_SECONDS
 * (default 60): past it, it interrupts the call (EINTR) and the path is reported stalled; if the call does not
 * return, it prints what the walk counted, the stalled path and a partial total, and exits 3 so the caller can
 * walk again without that path.
 *
 * Output, tab separated, one record per line:
 *     D  bytes  files  path                           a directory at depth <= DEPTH, its whole subtree
 *     E  errno  path                                 an entry it could not read (the first MAX_ERRORS)
 *     S  kind  path                                  a path not walked: tcc, excluded (-x) or stalled (all)
 *     T  bytes  files  dirs  errors  partial  shared_files  shared_saved_bytes  unmapped_files  skipped
 *     V  total  free  avail  important_avail  inodes_used   (-v: the volume; important_avail -1 when unknown)
 * partial is 1 when the time bound or a stall stopped the walk: every count is then a lower bound.
 * Build: cc -O2 -o glaeda-cdu glaeda-cdu.c -framework CoreFoundation   (macOS)
 */
#define _DEFAULT_SOURCE 1
#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <fcntl.h>
#include <fnmatch.h>
#include <fts.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <CoreFoundation/CoreFoundation.h>
#include <sys/attr.h>
#include <sys/mount.h>
#include <sys/param.h>
#else
#include <sys/statvfs.h>
#endif

#define MAX_ERRORS 50
#define MAX_EXCLUDES 256
#define CHUNK_BITS 20 /* one bitmap chunk covers 2^20 blocks (4 GiB at 4 KiB blocks) in 128 KiB */

static void die(const char *what) {
    fprintf(stderr, "glaeda-cdu: %s: %s\n", what, strerror(errno));
    exit(2);
}

/* ---- hard links: an open-addressing set of inode numbers (one device: the walk never crosses one) ---- */
static uint64_t *ino_set;
static size_t ino_cap, ino_len;

static int ino_insert(uint64_t ino) { /* 1 when new */
    if (ino == 0) ino = UINT64_MAX;
    if ((ino_len + 1) * 2 > ino_cap) {
        size_t cap = ino_cap ? ino_cap * 2 : 4096;
        uint64_t *next = calloc(cap, sizeof *next);
        if (!next) die("calloc");
        for (size_t i = 0; i < ino_cap; i++) {
            if (!ino_set[i]) continue;
            size_t j = (size_t)(ino_set[i] * 0x9E3779B97F4A7C15ULL) & (cap - 1);
            while (next[j]) j = (j + 1) & (cap - 1);
            next[j] = ino_set[i];
        }
        free(ino_set);
        ino_set = next;
        ino_cap = cap;
    }
    size_t j = (size_t)(ino * 0x9E3779B97F4A7C15ULL) & (ino_cap - 1);
    while (ino_set[j]) {
        if (ino_set[j] == ino) return 0;
        j = (j + 1) & (ino_cap - 1);
    }
    ino_set[j] = ino;
    ino_len++;
    return 1;
}

#if defined(__APPLE__)
/* ---- the block bitmap: sparse, chunks allocated on first touch ---- */
static uint8_t **chunks;
static size_t nchunks;

static uint64_t mark_blocks(uint64_t first, uint64_t count) { /* how many were not marked before */
    uint64_t fresh = 0;
    for (uint64_t b = first; b < first + count; b++) {
        size_t c = (size_t)(b >> CHUNK_BITS);
        if (c >= nchunks) {
            size_t n = nchunks ? nchunks : 64;
            while (n <= c) n *= 2;
            uint8_t **next = realloc(chunks, n * sizeof *next);
            if (!next) die("realloc");
            memset(next + nchunks, 0, (n - nchunks) * sizeof *next);
            chunks = next;
            nchunks = n;
        }
        if (!chunks[c] && !(chunks[c] = calloc((size_t)1 << (CHUNK_BITS - 3), 1))) die("calloc");
        uint64_t bit = b & ((1ULL << CHUNK_BITS) - 1);
        uint8_t m = (uint8_t)(1u << (bit & 7));
        if (!(chunks[c][bit >> 3] & m)) {
            chunks[c][bit >> 3] |= m;
            fresh++;
        }
    }
    return fresh;
}

static int may_share_blocks(const char *path) {
    struct attrlist al = {0};
    struct { uint32_t len; attribute_set_t returned; uint64_t ext_flags; } __attribute__((packed)) buf;
    al.bitmapcount = ATTR_BIT_MAP_COUNT;
    al.commonattr = ATTR_CMN_RETURNED_ATTRS;
    al.forkattr = ATTR_CMNEXT_EXT_FLAGS;
    if (getattrlist(path, &al, &buf, sizeof buf, FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED) != 0) return 0;
    if (!(buf.returned.forkattr & ATTR_CMNEXT_EXT_FLAGS)) return 0;
    return (buf.ext_flags & EF_MAY_SHARE_BLOCKS) != 0;
}

/* The file's blocks not yet counted, or -1 when its extents cannot be mapped (compressed, dataless, raced). */
static int64_t mapped_bytes(const char *path, off_t size, uint64_t bsize) {
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK);
    if (fd < 0) return -1;
    uint64_t fresh = 0;
    off_t off = 0;
    while (off < size) {
        struct log2phys l2p = {0};
        l2p.l2p_contigbytes = size - off;
        l2p.l2p_devoffset = off;
        if (fcntl(fd, F_LOG2PHYS_EXT, &l2p) != 0 || l2p.l2p_contigbytes <= 0) {
            close(fd);
            return -1;
        }
        uint64_t first = (uint64_t)l2p.l2p_devoffset / bsize;
        uint64_t last = ((uint64_t)l2p.l2p_devoffset + (uint64_t)l2p.l2p_contigbytes + bsize - 1) / bsize;
        fresh += mark_blocks(first, last - first);
        off += l2p.l2p_contigbytes;
    }
    close(fd);
    return (int64_t)(fresh * bsize);
}

static int64_t important_available(const char *path) {
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)path, (CFIndex)strlen(path), true);
    if (!url) return -1;
    int64_t out = -1;
    CFNumberRef n = NULL;
    if (CFURLCopyResourcePropertyForKey(url, kCFURLVolumeAvailableCapacityForImportantUsageKey, &n, NULL) && n) {
        long long v = 0;
        if (CFNumberGetValue(n, kCFNumberLongLongType, &v)) out = v;
    }
    if (n) CFRelease(n);
    CFRelease(url);
    return out;
}
#endif

/* ---- per-depth accumulators along the current path ---- */
struct frame { uint64_t bytes, files; char *path; };
static struct frame *stack;
static size_t stack_cap;
static uint64_t total_bytes, total_files; /* the root's frame as it closes */

static struct frame *frame_at(size_t level) {
    if (level >= stack_cap) {
        size_t n = stack_cap ? stack_cap * 2 : 64;
        while (n <= level) n *= 2;
        struct frame *next = realloc(stack, n * sizeof *next);
        if (!next) die("realloc");
        memset(next + stack_cap, 0, (n - stack_cap) * sizeof *next);
        stack = next;
        stack_cap = n;
    }
    return &stack[level];
}

static void print_path(const char *p) {
    for (; *p; p++) putchar(*p == '\n' || *p == '\t' ? '?' : *p);
}

/* ---- paths not walked ---- */
/* Relative to any "/Library/" in the path: folders macOS guards with TCC (privacy consent). */
static const char *const TCC_SKIP[] = {
    "News", "Mail", "Messages", "Safari", "Cookies", "Calendars", "Reminders", "HomeKit", "Suggestions",
    "IdentityServices", "Sharing", "Accounts", "PersonalizationPortrait", "Biome", "DuetExpertCenter",
    "Metadata/CoreSpotlight", "Autosave Information", "Containers/com.apple.*", "Group Containers/group.com.apple.*",
    "Group Containers/*.com.apple.*", "Daemon Containers", "Application Support/AddressBook",
    "Application Support/CallHistoryDB", "Application Support/CallHistoryTransactions",
    "Application Support/com.apple.TCC", "Application Support/FaceTime", "Application Support/Knowledge",
    "Application Support/com.apple.sharedfilelist", "Application Support/MobileSync", NULL,
};
static const char *excludes[MAX_EXCLUDES];
static int nexcludes;

static int tcc_protected(const char *path) {
    for (const char *at = strstr(path, "/Library/"); at; at = strstr(at + 1, "/Library/")) {
        const char *rest = at + strlen("/Library/");
        for (int i = 0; TCC_SKIP[i]; i++)
            if (fnmatch(TCC_SKIP[i], rest, 0) == 0) return 1;
    }
    return 0;
}

static int excluded(const char *path) {
    for (int i = 0; i < nexcludes; i++)
        if (strcmp(path, excludes[i]) == 0) return 1;
    return 0;
}

static void print_skip(const char *kind, const char *path) {
    printf("S\t%s\t", kind);
    print_path(path);
    putchar('\n');
}

static void close_frame(size_t level, long depth) {
    struct frame *f = frame_at(level);
    if ((long)level <= depth && f->path) {
        printf("D\t%llu\t%llu\t", (unsigned long long)f->bytes, (unsigned long long)f->files);
        print_path(f->path);
        putchar('\n');
    }
    if (level > 0) {
        frame_at(level - 1)->bytes += f->bytes;
        frame_at(level - 1)->files += f->files;
    } else {
        total_bytes = f->bytes;
        total_files = f->files;
    }
    free(f->path);
    f->path = NULL;
    f->bytes = f->files = 0;
}

/* ---- the per-call watchdog ---- */
static pthread_mutex_t walk_lock = PTHREAD_MUTEX_INITIALIZER; /* main holds it except inside a bounded call */
static pthread_t walker;
static char current[4096];          /* the directory being read, or the file being mapped */
static volatile time_t call_started; /* 0 outside a bounded call */
static volatile int call_interrupted;
static double dir_timeout = 60;
static long out_depth = 2;
static uint64_t dirs, errors, shared_files, shared_saved, unmapped, skipped;

static void print_total(int partial) {
    printf("T\t%llu\t%llu\t%llu\t%llu\t%d\t%llu\t%llu\t%llu\t%llu\n", (unsigned long long)total_bytes,
           (unsigned long long)total_files, (unsigned long long)dirs, (unsigned long long)errors, partial,
           (unsigned long long)shared_files, (unsigned long long)shared_saved, (unsigned long long)unmapped,
           (unsigned long long)skipped);
}

static void close_all(void) { /* every directory still open, deepest first: the counted part, a lower bound */
    for (size_t level = stack_cap; level-- > 0;)
        if (stack[level].path || stack[level].bytes || stack[level].files) close_frame(level, out_depth);
}

static void on_interrupt(int sig) { (void)sig; } /* only to make the blocked call return EINTR */

/* Enter and leave a call that may block in the kernel (a directory read, a file's getattrlist/open/fcntl). */
static void bounded_begin(const char *path) {
    if (path) {
        strncpy(current, path, sizeof current - 1);
        current[sizeof current - 1] = 0;
    }
    call_interrupted = 0;
    call_started = time(NULL);
    pthread_mutex_unlock(&walk_lock);
}

static int bounded_end(void) { /* 1 when the watchdog interrupted the call */
    pthread_mutex_lock(&walk_lock);
    call_started = 0;
    return call_interrupted;
}

static void *watchdog(void *unused) {
    (void)unused;
    for (;;) {
        sleep(1);
        time_t began = call_started;
        if (!began || difftime(time(NULL), began) < dir_timeout) continue;
        if (!call_interrupted) {
            call_interrupted = 1;
            pthread_kill(walker, SIGUSR1);
            continue;
        }
        if (difftime(time(NULL), began) < 2 * dir_timeout) continue;
        /* The call ignored the interrupt. The walker is blocked outside walk_lock, so its counts are stable. */
        if (pthread_mutex_trylock(&walk_lock) != 0) continue; /* it returned meanwhile */
        if (call_started != began) {
            pthread_mutex_unlock(&walk_lock);
            continue;
        }
        print_skip("stalled", current);
        skipped++;
        close_all();
        print_total(1);
        fflush(stdout);
        _exit(3);
    }
    return NULL;
}

int main(int argc, char **argv) {
    long depth = 2;
    double budget = 0;
    int volume = 0, opt;
    const char *usage = "usage: glaeda-cdu [-d DEPTH] [-t SECONDS] [-s DIR_SECONDS] [-x PATH]... [-v] ROOT\n";
    while ((opt = getopt(argc, argv, "d:t:s:x:v")) != -1) {
        if (opt == 'd') depth = strtol(optarg, NULL, 10);
        else if (opt == 't') budget = strtod(optarg, NULL);
        else if (opt == 's') dir_timeout = strtod(optarg, NULL);
        else if (opt == 'x' && nexcludes < MAX_EXCLUDES) excludes[nexcludes++] = optarg;
        else if (opt == 'v') volume = 1;
        else {
            fprintf(stderr, "%s", usage);
            return 2;
        }
    }
    if (optind != argc - 1) {
        fprintf(stderr, "%s", usage);
        return 2;
    }
    if (dir_timeout < 1) dir_timeout = 1;
    out_depth = depth;
    char *root = argv[optind];
    uint64_t bsize = 4096;
#if defined(__APPLE__)
    struct statfs vfs;
    if (statfs(root, &vfs) != 0) die(root);
    if (vfs.f_bsize > 0) bsize = vfs.f_bsize;
    if (volume)
        printf("V\t%llu\t%llu\t%llu\t%lld\t%llu\n", (unsigned long long)vfs.f_blocks * vfs.f_bsize,
               (unsigned long long)vfs.f_bfree * vfs.f_bsize, (unsigned long long)vfs.f_bavail * vfs.f_bsize,
               (long long)important_available(root), (unsigned long long)(vfs.f_files - vfs.f_ffree));
#else
    struct statvfs vfs;
    if (statvfs(root, &vfs) != 0) die(root);
    if (vfs.f_frsize > 0) bsize = vfs.f_frsize;
    if (volume)
        printf("V\t%llu\t%llu\t%llu\t-1\t%llu\n", (unsigned long long)vfs.f_blocks * vfs.f_frsize,
               (unsigned long long)vfs.f_bfree * vfs.f_frsize, (unsigned long long)vfs.f_bavail * vfs.f_frsize,
               (unsigned long long)(vfs.f_files - vfs.f_ffree));
#endif
    (void)bsize;
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);
    char *roots[] = {root, NULL};
    FTS *fts = fts_open(roots, FTS_PHYSICAL | FTS_XDEV | FTS_NOCHDIR, NULL);
    if (!fts) die("fts_open");
    uint64_t seen = 0;
    int partial = 0;
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_interrupt; /* no SA_RESTART: an interrupted read returns EINTR */
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);
    walker = pthread_self();
    pthread_mutex_lock(&walk_lock);
    pthread_t dog;
    if (pthread_create(&dog, NULL, watchdog, NULL) != 0) die("pthread_create");
    FTSENT *e;
    const char *reading = NULL; /* the directory the next fts_read reads (after its FTS_D) */
    for (;;) {
        errno = 0;
        bounded_begin(reading);
        e = fts_read(fts);
        int interrupted = bounded_end();
        if (interrupted && reading && e && e->fts_info != FTS_D) {
            print_skip("stalled", reading); /* the read returned EINTR: what fts reports next lacks that dir */
            skipped++;
        }
        reading = NULL;
        if (e == NULL) {
            if (errno != 0) partial = 1; /* fts_read's NULL with errno set is an incomplete walk, not its end */
            break;
        }
        if (budget > 0 && (++seen & 1023) == 0) {
            clock_gettime(CLOCK_MONOTONIC, &now);
            if ((double)(now.tv_sec - start.tv_sec) + (now.tv_nsec - start.tv_nsec) / 1e9 > budget) {
                partial = 1;
                break;
            }
        }
        size_t level = (size_t)e->fts_level;
        switch (e->fts_info) {
        case FTS_D: {
            struct frame *f = frame_at(level);
            f->bytes = (uint64_t)e->fts_statp->st_blocks * 512;
            f->files = 0;
            f->path = (long)level <= depth ? strdup(e->fts_path) : NULL;
            dirs++;
            int tcc = level > 0 && tcc_protected(e->fts_path);
            if (tcc || (level > 0 && excluded(e->fts_path))) {
                print_skip(tcc ? "tcc" : "excluded", e->fts_path);
                skipped++;
                fts_set(fts, e, FTS_SKIP); /* fts sends its FTS_DP next, which closes the frame */
            } else {
                reading = e->fts_path;
            }
            continue;
        }
        case FTS_DP:
            close_frame(level, depth);
            continue;
        case FTS_DNR:
        case FTS_ERR:
        case FTS_NS:
            if (e->fts_errno == EINTR) { /* the watchdog interrupted this read: reported stalled above */
                if (e->fts_info == FTS_DNR) close_frame(level, depth);
                continue;
            }
            if (errors++ < MAX_ERRORS) {
                printf("E\t%d\t", e->fts_errno);
                print_path(e->fts_path);
                putchar('\n');
            }
            /* fts returned this directory as FTS_D, could not open it, and sends no FTS_DP: close it here */
            if (e->fts_info == FTS_DNR) close_frame(level, depth);
            continue;
        case FTS_DC:
            continue;
        default:
            break;
        }
        struct stat *st = e->fts_statp;
        uint64_t bytes = (uint64_t)st->st_blocks * 512;
        if (st->st_nlink > 1 && !S_ISDIR(st->st_mode) && !ino_insert((uint64_t)st->st_ino)) bytes = 0;
#if defined(__APPLE__)
        else if (S_ISREG(st->st_mode) && st->st_size > 0) {
            bounded_begin(e->fts_path);
            int shares = may_share_blocks(e->fts_accpath);
            int64_t mapped = shares ? mapped_bytes(e->fts_accpath, st->st_size, bsize) : 0;
            if (bounded_end()) {
                print_skip("stalled", e->fts_path);
                skipped++;
            }
            if (!shares) goto counted;
            shared_files++;
            if (mapped < 0) {
                unmapped++;
            } else {
                if ((uint64_t)mapped < bytes) shared_saved += bytes - (uint64_t)mapped;
                bytes = (uint64_t)mapped;
            }
        }
counted:
#endif
        struct frame *parent = frame_at(level > 0 ? level - 1 : 0);
        parent->bytes += bytes;
        parent->files += 1;
    }
    close_all();
    print_total(partial);
    fflush(stdout);
    fts_close(fts);
    _exit(0); /* the watchdog thread is still sleeping */
}
