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
 *     glaeda-cdu [-d DEPTH] [-t SECONDS] [-v] ROOT
 *
 * Output, tab separated, one record per line:
 *     D  bytes  files  path                           a directory at depth <= DEPTH, its whole subtree
 *     E  errno  path                                 an entry it could not read (the first MAX_ERRORS)
 *     T  bytes  files  dirs  errors  partial  shared_files  shared_saved_bytes  unmapped_files
 *     V  total  free  avail  important_avail  inodes_used   (-v: the volume; important_avail -1 when unknown)
 * partial is 1 when the time bound stopped the walk: every count is then a lower bound.
 * Build: cc -O2 -o glaeda-cdu glaeda-cdu.c -framework CoreFoundation   (macOS)
 */
#define _DEFAULT_SOURCE 1
#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <fcntl.h>
#include <fts.h>
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

int main(int argc, char **argv) {
    long depth = 2;
    double budget = 0;
    int volume = 0, opt;
    while ((opt = getopt(argc, argv, "d:t:v")) != -1) {
        if (opt == 'd') depth = strtol(optarg, NULL, 10);
        else if (opt == 't') budget = strtod(optarg, NULL);
        else if (opt == 'v') volume = 1;
        else {
            fprintf(stderr, "usage: glaeda-cdu [-d DEPTH] [-t SECONDS] [-v] ROOT\n");
            return 2;
        }
    }
    if (optind != argc - 1) {
        fprintf(stderr, "usage: glaeda-cdu [-d DEPTH] [-t SECONDS] [-v] ROOT\n");
        return 2;
    }
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
    uint64_t dirs = 0, errors = 0, shared_files = 0, shared_saved = 0, unmapped = 0, seen = 0;
    int partial = 0;
    FTSENT *e;
    for (;;) {
        errno = 0;
        if ((e = fts_read(fts)) == NULL) {
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
            continue;
        }
        case FTS_DP:
            close_frame(level, depth);
            continue;
        case FTS_DNR:
        case FTS_ERR:
        case FTS_NS:
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
        else if (S_ISREG(st->st_mode) && st->st_size > 0 && may_share_blocks(e->fts_accpath)) {
            int64_t mapped = mapped_bytes(e->fts_accpath, st->st_size, bsize);
            shared_files++;
            if (mapped < 0) {
                unmapped++;
            } else {
                if ((uint64_t)mapped < bytes) shared_saved += bytes - (uint64_t)mapped;
                bytes = (uint64_t)mapped;
            }
        }
#endif
        struct frame *parent = frame_at(level > 0 ? level - 1 : 0);
        parent->bytes += bytes;
        parent->files += 1;
    }
    /* Close every directory still open (a stopped walk): the counted part, a lower bound. */
    for (size_t level = stack_cap; level-- > 0;) {
        if (stack[level].path || stack[level].bytes || stack[level].files) close_frame(level, depth);
    }
    printf("T\t%llu\t%llu\t%llu\t%llu\t%d\t%llu\t%llu\t%llu\n", (unsigned long long)total_bytes,
           (unsigned long long)total_files, (unsigned long long)dirs, (unsigned long long)errors, partial,
           (unsigned long long)shared_files, (unsigned long long)shared_saved, (unsigned long long)unmapped);
    fts_close(fts);
    return 0;
}
