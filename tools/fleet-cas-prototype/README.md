# Fleet compilation-cache prototype

Throwaway measurement tool for the fleet-shared compilation cache experiment
([`docs/experiments/fleet-compilation-cache-2026-09-23.md`](../../docs/experiments/fleet-compilation-cache-2026-09-23.md)).
It is not part of the `glaeda` crate, is not built by `scripts/verify`, and is
not a supported service.

It serves the two gRPC services Xcode's `COMPILATION_CACHE_REMOTE_SERVICE_PATH`
talks to, from swiftlang/llvm-project
`llvm/lib/RemoteCachingService/RemoteCacheProto` (Apache-2.0 WITH
LLVM-exception, copied unmodified into `proto/`):

- `compilation_cache_service.cas.v1.CASDBService`: content-addressed objects.
  The server computes every ID from the object's bytes and references, so an
  upload cannot claim someone else's ID. Damaged objects read as misses and a
  re-upload replaces them.
- `compilation_cache_service.keyvalue.v1.KeyValueDB`: cache key to result.
  This is the only poisoning surface. `--read-only-kv` refuses every write.
  Existing entries are never replaced (first writer wins).

Storage is one file per object under the store directory. Counters go to
`<store>/stats.json`.

```sh
cargo build --release
target/release/fleet-cas /tmp/fcas.sock /path/to/store [--read-only-kv]

PKG=... SCHEME=... WORK=... scripts/xcode-cache-build.sh fill \
  COMPILATION_CACHE_ENABLE_PLUGIN=YES \
  COMPILATION_CACHE_REMOTE_SERVICE_PATH=/tmp/fcas.sock
```

Keep the socket path short; macOS limits unix socket paths to 104 bytes.

Known prototype gaps, all measured in the experiment: it must run on the same
host as the build (the client sends large blobs as local file paths, and the
prototype reads whatever path it is given, so run it only for your own user),
it does not require a KV entry's objects to be present before accepting the
entry, and it has no size budget or eviction.
