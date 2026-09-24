//! Prototype fleet compilation-cache server for Xcode's
//! COMPILATION_CACHE_REMOTE_SERVICE_PATH (LLVM compilation_cache_service
//! protocol). Disk-backed CAS + KV behind a unix socket or a TCP port.
//!
//! Trust model under test: CAS object IDs are computed here from content, so
//! CAS writes cannot impersonate. KV writes (cache key -> result) are the only
//! poisoning surface; `--read-only-kv` refuses them.
//!
//! Two roles, one binary:
//! - fleet store: `fleet-cas tcp:<addr> <store>`, reached over the network.
//! - node daemon: `fleet-cas <socket> <store> --upstream http://<addr>`. Xcode
//!   talks to it on the same host (the client sends large blobs as local file
//!   paths). Reads try the local store, then the fleet store, and keep what
//!   they fetch. Writes land locally and are forwarded before they are
//!   acknowledged, so an index entry never reaches the fleet store ahead of
//!   the objects it names. With `--read-only-kv` nothing is forwarded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use prost::Message;
use sha2::{Digest, Sha256};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

pub mod cas {
    tonic::include_proto!("compilation_cache_service.cas.v1");
}
pub mod kv {
    tonic::include_proto!("compilation_cache_service.keyvalue.v1");
}
pub mod fleet {
    tonic::include_proto!("glaeda.fleetcas.v1");
}

#[derive(Default)]
struct Stats {
    cas_put: AtomicU64,
    cas_put_new: AtomicU64,
    cas_get_hit: AtomicU64,
    cas_get_miss: AtomicU64,
    kv_get_hit: AtomicU64,
    kv_get_miss: AtomicU64,
    kv_put: AtomicU64,
    kv_put_refused: AtomicU64,
    kv_put_conflict: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    /// KV entries whose 32-byte values name objects this store lacks.
    kv_put_dangling: AtomicU64,
    // Node daemon only: traffic to the fleet store.
    up_cas_fetch: AtomicU64,
    up_cas_fetch_miss: AtomicU64,
    up_kv_fetch: AtomicU64,
    up_kv_fetch_miss: AtomicU64,
    up_cas_push: AtomicU64,
    up_kv_push: AtomicU64,
    up_bytes_fetched: AtomicU64,
    up_calls: AtomicU64,
    up_micros: AtomicU64,
    /// Fleet-store calls that failed; reads were answered as local misses.
    up_errors: AtomicU64,
    /// Fleet-store calls skipped during the backoff after a failure.
    up_skipped: AtomicU64,
    /// Fetched objects whose recomputed ID did not match (answered as misses).
    up_cas_verify_fail: AtomicU64,
    /// Closure prefetches: calls, objects kept, bytes kept.
    up_prefetch_calls: AtomicU64,
    up_prefetched: AtomicU64,
    up_prefetch_bytes: AtomicU64,
}

impl Stats {
    fn line(&self) -> String {
        let fields = [
            ("cas_put", &self.cas_put),
            ("cas_put_new", &self.cas_put_new),
            ("cas_get_hit", &self.cas_get_hit),
            ("cas_get_miss", &self.cas_get_miss),
            ("kv_get_hit", &self.kv_get_hit),
            ("kv_get_miss", &self.kv_get_miss),
            ("kv_put", &self.kv_put),
            ("kv_put_refused", &self.kv_put_refused),
            ("kv_put_conflict", &self.kv_put_conflict),
            ("kv_put_dangling", &self.kv_put_dangling),
            ("bytes_in", &self.bytes_in),
            ("bytes_out", &self.bytes_out),
            ("up_cas_fetch", &self.up_cas_fetch),
            ("up_cas_fetch_miss", &self.up_cas_fetch_miss),
            ("up_kv_fetch", &self.up_kv_fetch),
            ("up_kv_fetch_miss", &self.up_kv_fetch_miss),
            ("up_cas_push", &self.up_cas_push),
            ("up_kv_push", &self.up_kv_push),
            ("up_bytes_fetched", &self.up_bytes_fetched),
            ("up_calls", &self.up_calls),
            ("up_micros", &self.up_micros),
            ("up_errors", &self.up_errors),
            ("up_skipped", &self.up_skipped),
            ("up_cas_verify_fail", &self.up_cas_verify_fail),
            ("up_prefetch_calls", &self.up_prefetch_calls),
            ("up_prefetched", &self.up_prefetched),
            ("up_prefetch_bytes", &self.up_prefetch_bytes),
        ];
        let body: Vec<String> = fields
            .iter()
            .map(|(k, v)| format!("\"{k}\":{}", v.load(Relaxed)))
            .collect();
        format!("{{{}}}", body.join(","))
    }

    /// Time one call to the fleet store.
    async fn upstream<T>(&self, f: impl std::future::Future<Output = T>) -> T {
        let t = Instant::now();
        let r = f.await;
        self.up_calls.fetch_add(1, Relaxed);
        self.up_micros
            .fetch_add(t.elapsed().as_micros() as u64, Relaxed);
        r
    }
}

struct Upstream {
    cas: cas::casdb_service_client::CasdbServiceClient<Channel>,
    kv: kv::key_value_db_client::KeyValueDbClient<Channel>,
    fleet: fleet::fleet_cas_client::FleetCasClient<Channel>,
    /// Cleared if the fleet store does not serve GetClosure.
    closure: AtomicBool,
}

struct Store {
    root: PathBuf,
    read_only_kv: bool,
    /// Who may write over TCP: `None` for a unix-socket node (its clients
    /// are local builds), else the allowed peer addresses. The fleet store
    /// has no other authentication, so an empty list makes it read-only.
    writers: Option<Vec<std::net::IpAddr>>,
    /// Accept `CASBytes.file_path` uploads. Only a node daemon, whose clients
    /// are local builds, may read paths it is handed; a network-facing store
    /// would read any file on its host for whoever connects.
    allow_file_paths: bool,
    upstream: Option<Upstream>,
    /// Unix millis until which fleet-store calls are skipped after a failure.
    upstream_down_until: AtomicU64,
    stats: Stats,
}

const UPSTREAM_BACKOFF_MS: u64 = 30_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn blob_data(obj: &cas::CasObject) -> &[u8] {
    match obj.blob.as_ref().and_then(|b| b.contents.as_ref()) {
        Some(cas::cas_bytes::Contents::Data(d)) => d.as_slice(),
        _ => &[],
    }
}

fn data_object(refs: Vec<cas::CasDataId>, data: Vec<u8>) -> cas::CasObject {
    cas::CasObject {
        blob: Some(cas::CasBytes {
            contents: Some(cas::cas_bytes::Contents::Data(data)),
        }),
        references: refs,
    }
}

/// The store IDs an index entry names. Xcode's value is its own nested
/// protobuf record in which each output appears as a pair: the plugin's
/// 65-byte ID and this store's 32-byte ID. Walk the wire format and collect
/// every 32-byte length-delimited field; a false positive only costs a lookup
/// of an absent ID.
fn embedded_ids(value: &kv::Value) -> Vec<Vec<u8>> {
    fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
        let mut r = 0u64;
        for shift in (0..64).step_by(7) {
            let c = *b.get(*i)?;
            *i += 1;
            r |= u64::from(c & 0x7f) << shift;
            if c < 0x80 {
                return Some(r);
            }
        }
        None
    }
    // Returns false if `b` is not a well-formed message, so a string that
    // happens to start like one is not mined for IDs.
    fn walk(b: &[u8], depth: u32, out: &mut Vec<Vec<u8>>) -> bool {
        let mut i = 0;
        let mut found = Vec::new();
        while i < b.len() {
            let Some(key) = varint(b, &mut i) else {
                return false;
            };
            match key & 7 {
                0 => {
                    if varint(b, &mut i).is_none() {
                        return false;
                    }
                }
                1 => i += 8,
                5 => i += 4,
                2 => {
                    let Some(len) = varint(b, &mut i) else {
                        return false;
                    };
                    let Some(end) = i.checked_add(len as usize).filter(|&e| e <= b.len()) else {
                        return false;
                    };
                    let field = &b[i..end];
                    if field.len() == 32 {
                        found.push(field.to_vec());
                    } else if depth < 8 {
                        walk(field, depth + 1, &mut found);
                    }
                    i = end;
                }
                _ => return false,
            }
        }
        if i != b.len() {
            return false;
        }
        out.extend(found);
        true
    }
    let mut out = Vec::new();
    for v in value.entries.values() {
        if v.len() == 32 {
            out.push(v.clone());
        } else {
            walk(v, 0, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn upstream_err(e: Status) -> Status {
    Status::unavailable(format!("fleet store: {}", e.message()))
}

fn object_id(refs: &[cas::CasDataId], data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"glaeda-fleet-cas-v1\0");
    h.update((refs.len() as u64).to_le_bytes());
    for r in refs {
        h.update((r.id.len() as u64).to_le_bytes());
        h.update(&r.id);
    }
    h.update((data.len() as u64).to_le_bytes());
    h.update(data);
    h.finalize().to_vec()
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(path: &Path) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Relaxed);
    path.with_extension(format!("tmp{}.{seq}", std::process::id()))
}

/// Replace `path` atomically. Used for CAS objects, where any intact copy is
/// equivalent, so a concurrent writer winning is harmless.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path(path);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Publish `path` only if it does not exist yet (hard link fails on an
/// existing target), so concurrent KV writers cannot replace the first entry.
fn publish_new(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let tmp = temp_path(path);
    std::fs::write(&tmp, bytes)?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

impl Store {
    /// Refuse a write from a TCP peer that is not an allowed writer.
    fn check_writer<T>(&self, r: &Request<T>) -> Result<(), Status> {
        let Some(allowed) = &self.writers else {
            return Ok(());
        };
        match r.remote_addr() {
            Some(peer) if allowed.contains(&peer.ip()) => Ok(()),
            _ => {
                self.stats.kv_put_refused.fetch_add(1, Relaxed);
                Err(Status::permission_denied("not a fleet-cas writer"))
            }
        }
    }

    fn cas_path(&self, id: &[u8]) -> PathBuf {
        let h = hex::encode(id);
        self.root.join("cas").join(&h[..2]).join(h)
    }

    fn kv_path(&self, key: &[u8]) -> PathBuf {
        let h = hex::encode(Sha256::digest(key));
        self.root.join("kv").join(&h[..2]).join(h)
    }

    fn bytes_of(&self, b: Option<cas::CasBytes>) -> Result<Vec<u8>, Status> {
        match b.and_then(|b| b.contents) {
            None => Ok(Vec::new()),
            Some(cas::cas_bytes::Contents::Data(d)) => Ok(d),
            Some(cas::cas_bytes::Contents::FilePath(_)) if !self.allow_file_paths => Err(
                Status::invalid_argument("file_path uploads are accepted only on the build host"),
            ),
            Some(cas::cas_bytes::Contents::FilePath(p)) => {
                std::fs::read(&p).map_err(|e| Status::invalid_argument(format!("read {p}: {e}")))
            }
        }
    }

    fn put(&self, refs: Vec<cas::CasDataId>, data: Vec<u8>) -> Result<Vec<u8>, Status> {
        let id = object_id(&refs, &data);
        self.stats.cas_put.fetch_add(1, Relaxed);
        self.stats.bytes_in.fetch_add(data.len() as u64, Relaxed);
        let path = self.cas_path(&id);
        // Rewrite when absent or damaged, so a re-upload repairs corruption.
        if !self.intact(&path, &id) {
            let obj = data_object(refs, data);
            std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|_| write_atomic(&path, &obj.encode_to_vec()))
                .map_err(|e| Status::internal(e.to_string()))?;
            self.stats.cas_put_new.fetch_add(1, Relaxed);
        }
        Ok(id)
    }

    fn intact(&self, path: &Path, id: &[u8]) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(obj) = cas::CasObject::decode(bytes.as_slice()) else {
            return false;
        };
        object_id(&obj.references, blob_data(&obj)) == id
    }

    fn get(&self, id: &[u8]) -> Result<Option<cas::CasObject>, Status> {
        // Every ID this store issues is a SHA-256 digest; anything else is a miss.
        if id.len() != 32 {
            self.stats.cas_get_miss.fetch_add(1, Relaxed);
            return Ok(None);
        }
        match std::fs::read(self.cas_path(id)) {
            Ok(bytes) => {
                // Corruption is a miss, never a wrong answer.
                let Ok(obj) = cas::CasObject::decode(bytes.as_slice()) else {
                    self.stats.cas_get_miss.fetch_add(1, Relaxed);
                    return Ok(None);
                };
                let data = blob_data(&obj);
                if object_id(&obj.references, data) != id {
                    self.stats.cas_get_miss.fetch_add(1, Relaxed);
                    return Ok(None);
                }
                self.stats.cas_get_hit.fetch_add(1, Relaxed);
                self.stats.bytes_out.fetch_add(data.len() as u64, Relaxed);
                Ok(Some(obj))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.stats.cas_get_miss.fetch_add(1, Relaxed);
                Ok(None)
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }
}

impl Store {
    fn upstream_backing_off(&self) -> bool {
        let skip = now_ms() < self.upstream_down_until.load(Relaxed);
        if skip {
            self.stats.up_skipped.fetch_add(1, Relaxed);
        }
        skip
    }

    /// The fleet store to read from, unless a recent call failed: then every
    /// lookup is a local miss for a while instead of a timeout each.
    fn readable_upstream(&self) -> Option<&Upstream> {
        let up = self.upstream.as_ref()?;
        (!self.upstream_backing_off()).then_some(up)
    }

    fn upstream_failed(&self) {
        self.stats.up_errors.fetch_add(1, Relaxed);
        self.upstream_down_until
            .store(now_ms() + UPSTREAM_BACKOFF_MS, Relaxed);
    }

    /// A forwarded write failed or was skipped: the build gets an error (and
    /// compiles on), and nothing is published half-way.
    fn write_unavailable(&self, e: Option<Status>) -> Status {
        if let Some(e) = e {
            self.upstream_failed();
            return upstream_err(e);
        }
        Status::unavailable("fleet store unavailable (backing off)")
    }

    /// Local object, else fetch it from the fleet store, verify it, keep it.
    async fn get_or_fetch(&self, id: &[u8]) -> Result<Option<cas::CasObject>, Status> {
        if let Some(obj) = self.get(id)? {
            return Ok(Some(obj));
        }
        // Every ID the fleet store issues is a SHA-256 digest.
        if id.len() != 32 {
            return Ok(None);
        }
        let Some(up) = self.readable_upstream() else {
            return Ok(None);
        };
        let req = cas::CasGetRequest {
            cas_id: Some(cas::CasDataId { id: id.to_vec() }),
            write_to_disk: false,
        };
        // An unreachable fleet store degrades to a local miss: the build
        // compiles instead of failing or waiting.
        let Ok(resp) = self.stats.upstream(up.cas.clone().get(req)).await else {
            self.upstream_failed();
            return Ok(None);
        };
        let Some(cas::cas_get_response::Contents::Data(obj)) = resp.into_inner().contents else {
            self.stats.up_cas_fetch_miss.fetch_add(1, Relaxed);
            return Ok(None);
        };
        // The fleet store is not trusted for content: recompute the ID, and
        // hand Xcode an object rebuilt from the verified bytes only.
        let data = blob_data(&obj).to_vec();
        if object_id(&obj.references, &data) != id {
            self.stats.up_cas_verify_fail.fetch_add(1, Relaxed);
            return Ok(None);
        }
        self.stats.up_cas_fetch.fetch_add(1, Relaxed);
        self.stats
            .up_bytes_fetched
            .fetch_add(data.len() as u64, Relaxed);
        self.put(obj.references.clone(), data.clone())?;
        Ok(Some(data_object(obj.references, data)))
    }

    /// Store locally, then forward to the fleet store before acknowledging.
    async fn put_through(
        &self,
        refs: Vec<cas::CasDataId>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, Status> {
        let forward = match &self.upstream {
            Some(up) if !self.read_only_kv => Some((up, data_object(refs.clone(), data.clone()))),
            _ => None,
        };
        let id = self.put(refs, data)?;
        if let Some((up, obj)) = forward {
            if self.upstream_backing_off() {
                return Err(self.write_unavailable(None));
            }
            let req = cas::CasPutRequest { data: Some(obj) };
            let resp = self
                .stats
                .upstream(up.cas.clone().put(req))
                .await
                .map_err(|e| self.write_unavailable(Some(e)))?
                .into_inner();
            match resp.contents {
                Some(cas::cas_put_response::Contents::CasId(c)) if c.id == id => {
                    self.stats.up_cas_push.fetch_add(1, Relaxed);
                }
                Some(cas::cas_put_response::Contents::Error(e)) => {
                    return Err(Status::unavailable(e.description));
                }
                _ => return Err(Status::internal("fleet store returned a different ID")),
            }
        }
        Ok(id)
    }

    /// After fetching an index entry, pull every object it reaches in one
    /// streamed call, so Xcode's per-object loads that follow are local hits
    /// instead of one fleet round trip each. Best effort: anything missed
    /// here is still fetched object by object.
    async fn prefetch(&self, value: &kv::Value) {
        // The channel's per-call timeout covers only the response headers,
        // not a streamed body: bound the whole prefetch, so a store that
        // stalls mid-stream costs one timeout, then the backoff.
        let limit = std::time::Duration::from_secs(30);
        if tokio::time::timeout(limit, self.prefetch_inner(value))
            .await
            .is_err()
        {
            self.upstream_failed();
        }
    }

    async fn prefetch_inner(&self, value: &kv::Value) {
        let Some(up) = &self.upstream else { return };
        if !up.closure.load(Relaxed) {
            return;
        }
        let roots: Vec<Vec<u8>> = embedded_ids(value)
            .into_iter()
            .filter(|id| !self.cas_path(id).exists())
            .collect();
        if roots.is_empty() {
            return;
        }
        let t = Instant::now();
        let mut client = up.fleet.clone();
        let mut stream = match client.get_closure(fleet::ClosureRequest { roots }).await {
            Ok(r) => r.into_inner(),
            Err(e) if e.code() == tonic::Code::Unimplemented => {
                up.closure.store(false, Relaxed);
                return;
            }
            // A busy or refusing store: skip this prefetch only; the objects
            // still come one by one.
            Err(e)
                if matches!(
                    e.code(),
                    tonic::Code::ResourceExhausted | tonic::Code::InvalidArgument
                ) =>
            {
                return;
            }
            Err(_) => {
                self.upstream_failed();
                return;
            }
        };
        self.stats.up_prefetch_calls.fetch_add(1, Relaxed);
        while let Ok(Some(m)) = stream.message().await {
            let Ok(obj) = cas::CasObject::decode(m.object.as_slice()) else {
                self.stats.up_cas_verify_fail.fetch_add(1, Relaxed);
                continue;
            };
            let data = blob_data(&obj).to_vec();
            if object_id(&obj.references, &data) != m.id {
                self.stats.up_cas_verify_fail.fetch_add(1, Relaxed);
                continue;
            }
            if self.cas_path(&m.id).exists() {
                continue;
            }
            let len = data.len() as u64;
            if self.put(obj.references, data).is_ok() {
                self.stats.up_prefetched.fetch_add(1, Relaxed);
                self.stats.up_prefetch_bytes.fetch_add(len, Relaxed);
            }
        }
        self.stats.up_calls.fetch_add(1, Relaxed);
        self.stats
            .up_micros
            .fetch_add(t.elapsed().as_micros() as u64, Relaxed);
    }

    /// Count entries that name absent objects (the M3 publication rule,
    /// measured here, not enforced yet). Any 32-byte field in the record
    /// counts as an ID, so this can over-count, never under-count.
    fn count_dangling(&self, value: &kv::Value) {
        let dangling = embedded_ids(value)
            .iter()
            .any(|id| !self.cas_path(id).exists());
        if dangling {
            self.stats.kv_put_dangling.fetch_add(1, Relaxed);
        }
    }
}

struct CasSvc(Arc<Store>);
struct KvSvc(Arc<Store>);

#[tonic::async_trait]
impl cas::casdb_service_server::CasdbService for CasSvc {
    async fn put(
        &self,
        r: Request<cas::CasPutRequest>,
    ) -> Result<Response<cas::CasPutResponse>, Status> {
        self.0.check_writer(&r)?;
        let obj = r.into_inner().data.unwrap_or_default();
        let data = self.0.bytes_of(obj.blob)?;
        let id = self.0.put_through(obj.references, data).await?;
        Ok(Response::new(cas::CasPutResponse {
            contents: Some(cas::cas_put_response::Contents::CasId(cas::CasDataId {
                id,
            })),
        }))
    }

    async fn get(
        &self,
        r: Request<cas::CasGetRequest>,
    ) -> Result<Response<cas::CasGetResponse>, Status> {
        let id = r.into_inner().cas_id.unwrap_or_default().id;
        Ok(Response::new(match self.0.get_or_fetch(&id).await? {
            Some(obj) => cas::CasGetResponse {
                outcome: cas::cas_get_response::Outcome::Success as i32,
                contents: Some(cas::cas_get_response::Contents::Data(obj)),
            },
            None => cas::CasGetResponse {
                outcome: cas::cas_get_response::Outcome::ObjectNotFound as i32,
                contents: None,
            },
        }))
    }

    async fn save(
        &self,
        r: Request<cas::CasSaveRequest>,
    ) -> Result<Response<cas::CasSaveResponse>, Status> {
        self.0.check_writer(&r)?;
        let blob = r.into_inner().data.unwrap_or_default();
        let data = self.0.bytes_of(blob.blob)?;
        let id = self.0.put_through(Vec::new(), data).await?;
        Ok(Response::new(cas::CasSaveResponse {
            contents: Some(cas::cas_save_response::Contents::CasId(cas::CasDataId {
                id,
            })),
        }))
    }

    async fn load(
        &self,
        r: Request<cas::CasLoadRequest>,
    ) -> Result<Response<cas::CasLoadResponse>, Status> {
        let id = r.into_inner().cas_id.unwrap_or_default().id;
        Ok(Response::new(match self.0.get_or_fetch(&id).await? {
            Some(obj) => cas::CasLoadResponse {
                outcome: cas::cas_load_response::Outcome::Success as i32,
                contents: Some(cas::cas_load_response::Contents::Data(cas::CasBlob {
                    blob: obj.blob,
                })),
            },
            None => cas::CasLoadResponse {
                outcome: cas::cas_load_response::Outcome::ObjectNotFound as i32,
                contents: None,
            },
        }))
    }
}

fn kv_response(value: Option<kv::Value>) -> Response<kv::GetValueResponse> {
    Response::new(match value {
        Some(v) => kv::GetValueResponse {
            outcome: kv::get_value_response::Outcome::Success as i32,
            contents: Some(kv::get_value_response::Contents::Value(v)),
        },
        None => kv::GetValueResponse {
            outcome: kv::get_value_response::Outcome::KeyNotFound as i32,
            contents: None,
        },
    })
}

#[tonic::async_trait]
impl kv::key_value_db_server::KeyValueDb for KvSvc {
    async fn get_value(
        &self,
        r: Request<kv::GetValueRequest>,
    ) -> Result<Response<kv::GetValueResponse>, Status> {
        let key = r.into_inner().key;
        let s = &self.0;
        let path = s.kv_path(&key);
        match std::fs::read(&path) {
            Ok(bytes) => {
                // A damaged entry is a miss, like a damaged object.
                if let Ok(value) = kv::Value::decode(bytes.as_slice()) {
                    s.stats.kv_get_hit.fetch_add(1, Relaxed);
                    return Ok(kv_response(Some(value)));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Status::internal(e.to_string())),
        }
        s.stats.kv_get_miss.fetch_add(1, Relaxed);
        let Some(up) = s.readable_upstream() else {
            return Ok(kv_response(None));
        };
        let mut client = up.kv.clone();
        let fetch = client.get_value(kv::GetValueRequest { key });
        let Ok(resp) = s.stats.upstream(fetch).await else {
            s.upstream_failed();
            return Ok(kv_response(None));
        };
        let Some(kv::get_value_response::Contents::Value(value)) = resp.into_inner().contents
        else {
            s.stats.up_kv_fetch_miss.fetch_add(1, Relaxed);
            return Ok(kv_response(None));
        };
        s.stats.up_kv_fetch.fetch_add(1, Relaxed);
        s.prefetch(&value).await;
        // Keep it: the next lookup on this node is local.
        let _ = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|_| publish_new(&path, &value.encode_to_vec()));
        Ok(kv_response(Some(value)))
    }

    async fn put_value(
        &self,
        r: Request<kv::PutValueRequest>,
    ) -> Result<Response<kv::PutValueResponse>, Status> {
        let s = &self.0;
        s.check_writer(&r)?;
        if s.read_only_kv {
            s.stats.kv_put_refused.fetch_add(1, Relaxed);
            return Ok(Response::new(kv::PutValueResponse {
                error: Some(kv::ResponseError {
                    description: "read-only cache client".into(),
                }),
            }));
        }
        let req = r.into_inner();
        let value = req.value.unwrap_or_default();
        s.count_dangling(&value);
        // Forward first: this node's copy must not claim an entry the fleet
        // store refused. (If the fleet store already held a different value,
        // it keeps its own and this node keeps the one it computed; both came
        // from real compiles.)
        if let Some(up) = &s.upstream {
            if s.upstream_backing_off() {
                return Err(s.write_unavailable(None));
            }
            let fwd = kv::PutValueRequest {
                key: req.key.clone(),
                value: Some(value.clone()),
            };
            let resp = s
                .stats
                .upstream(up.kv.clone().put_value(fwd))
                .await
                .map_err(|e| s.write_unavailable(Some(e)))?
                .into_inner();
            if resp.error.is_some() {
                return Ok(Response::new(resp));
            }
            s.stats.up_kv_push.fetch_add(1, Relaxed);
        }
        let bytes = value.encode_to_vec();
        let path = s.kv_path(&req.key);
        s.stats.kv_put.fetch_add(1, Relaxed);
        // First writer wins, including under concurrency: an existing mapping
        // is never replaced, and a differing write is only counted.
        let published = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|_| publish_new(&path, &bytes))
            .map_err(|e| Status::internal(e.to_string()))?;
        if !published && std::fs::read(&path).is_ok_and(|existing| existing != bytes) {
            s.stats.kv_put_conflict.fetch_add(1, Relaxed);
        }
        Ok(Response::new(kv::PutValueResponse { error: None }))
    }
}

struct FleetSvc(Arc<Store>);

/// Bounds on the unauthenticated closure RPC: roots per call, and walks in
/// flight (each holds a blocking-pool thread while it streams).
const MAX_CLOSURE_ROOTS: usize = 4096;
static CLOSURE_SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(64)));

#[tonic::async_trait]
impl fleet::fleet_cas_server::FleetCas for FleetSvc {
    type GetClosureStream =
        tokio_stream::wrappers::ReceiverStream<Result<fleet::ClosureObject, Status>>;

    async fn get_closure(
        &self,
        r: Request<fleet::ClosureRequest>,
    ) -> Result<Response<Self::GetClosureStream>, Status> {
        let mut queue = r.into_inner().roots;
        queue.sort();
        queue.dedup();
        if queue.len() > MAX_CLOSURE_ROOTS {
            return Err(Status::invalid_argument("too many closure roots"));
        }
        let permit = CLOSURE_SLOTS
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("closure walks busy"))?;
        let store = self.0.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut seen: HashSet<Vec<u8>> = queue.iter().cloned().collect();
            while let Some(id) = queue.pop() {
                // `get` verifies the object and treats damage as absence.
                let Ok(Some(obj)) = store.get(&id) else {
                    continue;
                };
                for r in &obj.references {
                    if seen.insert(r.id.clone()) {
                        queue.push(r.id.clone());
                    }
                }
                let msg = fleet::ClosureObject {
                    id,
                    object: obj.encode_to_vec(),
                };
                if tx.blocking_send(Ok(msg)).is_err() {
                    return;
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

const USAGE: &str = "usage: fleet-cas <socket | tcp:ADDR> <store> [--read-only-kv] [--upstream http://HOST:PORT] [--no-prefetch] [--writers IP,IP]";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(listen), Some(root)) = (args.first(), args.get(1)) else {
        return Err(USAGE.into());
    };
    let root = PathBuf::from(root);
    let mut read_only_kv = false;
    let mut upstream_url = None;
    let mut prefetch = true;
    let mut writer_list: Vec<std::net::IpAddr> = Vec::new();
    let mut rest = args[2..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--read-only-kv" => read_only_kv = true,
            "--no-prefetch" => prefetch = false,
            "--writers" => {
                for ip in rest
                    .next()
                    .ok_or(USAGE)?
                    .split(',')
                    .filter(|s| !s.is_empty())
                {
                    writer_list.push(ip.parse()?);
                }
            }
            "--upstream" => upstream_url = Some(rest.next().ok_or(USAGE)?.clone()),
            _ => return Err(USAGE.into()),
        }
    }
    std::fs::create_dir_all(&root)?;

    let upstream = match &upstream_url {
        Some(url) => {
            // Lazy: the node starts (and serves local hits) while the fleet
            // store is down. Each call is bounded, and after a failure calls
            // are skipped for UPSTREAM_BACKOFF_MS, so a dead store costs a
            // build at most one timeout per backoff window.
            let ch = tonic::transport::Endpoint::from_shared(url.clone())?
                .tcp_nodelay(true)
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(30))
                .http2_keep_alive_interval(std::time::Duration::from_secs(10))
                .keep_alive_timeout(std::time::Duration::from_secs(5))
                .keep_alive_while_idle(true)
                .connect_lazy();
            Some(Upstream {
                cas: cas::casdb_service_client::CasdbServiceClient::new(ch.clone())
                    .max_decoding_message_size(512 << 20)
                    .max_encoding_message_size(512 << 20),
                kv: kv::key_value_db_client::KeyValueDbClient::new(ch.clone()),
                fleet: fleet::fleet_cas_client::FleetCasClient::new(ch)
                    .max_decoding_message_size(512 << 20),
                closure: AtomicBool::new(prefetch),
            })
        }
        None => None,
    };
    let store = Arc::new(Store {
        root: root.clone(),
        read_only_kv,
        allow_file_paths: !listen.starts_with("tcp:"),
        writers: listen.starts_with("tcp:").then_some(writer_list),
        upstream,
        upstream_down_until: AtomicU64::new(0),
        stats: Stats::default(),
    });

    let stats_store = store.clone();
    let stats_path = root.join("stats.json");
    tokio::spawn(async move {
        let mut last = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let line = stats_store.stats.line();
            if line != last {
                let _ = std::fs::write(&stats_path, &line);
                last = line;
            }
        }
    });

    eprintln!(
        "fleet-cas listening on {listen} store={} read_only_kv={read_only_kv} upstream={} writers={:?}",
        root.display(),
        upstream_url.as_deref().unwrap_or("none"),
        store.writers
    );
    let final_store = store.clone();
    let router = tonic::transport::Server::builder()
        .tcp_nodelay(true)
        .http2_keepalive_interval(Some(std::time::Duration::from_secs(10)))
        .http2_keepalive_timeout(Some(std::time::Duration::from_secs(5)))
        .add_service(
            cas::casdb_service_server::CasdbServiceServer::new(CasSvc(store.clone()))
                .max_decoding_message_size(512 << 20)
                .max_encoding_message_size(512 << 20),
        )
        .add_service(kv::key_value_db_server::KeyValueDbServer::new(KvSvc(
            store.clone(),
        )))
        .add_service(
            fleet::fleet_cas_server::FleetCasServer::new(FleetSvc(store))
                .max_encoding_message_size(512 << 20),
        );
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Some(addr) = listen.strip_prefix("tcp:") {
        router.serve_with_shutdown(addr.parse()?, shutdown).await?;
    } else {
        let socket = PathBuf::from(listen);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        router
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown)
            .await?;
    }
    eprintln!("{}", final_store.stats.line());
    Ok(())
}
