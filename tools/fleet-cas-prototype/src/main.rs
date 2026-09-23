//! Prototype fleet compilation-cache server for Xcode's
//! COMPILATION_CACHE_REMOTE_SERVICE_PATH (LLVM compilation_cache_service
//! protocol). Disk-backed CAS + KV behind a unix socket.
//!
//! Trust model under test: CAS object IDs are computed here from content, so
//! CAS writes cannot impersonate. KV writes (cache key -> result) are the only
//! poisoning surface; `--read-only-kv` refuses them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use prost::Message;
use sha2::{Digest, Sha256};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

pub mod cas {
    tonic::include_proto!("compilation_cache_service.cas.v1");
}
pub mod kv {
    tonic::include_proto!("compilation_cache_service.keyvalue.v1");
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
}

impl Stats {
    fn line(&self) -> String {
        format!(
            "{{\"cas_put\":{},\"cas_put_new\":{},\"cas_get_hit\":{},\"cas_get_miss\":{},\"kv_get_hit\":{},\"kv_get_miss\":{},\"kv_put\":{},\"kv_put_refused\":{},\"kv_put_conflict\":{},\"bytes_in\":{},\"bytes_out\":{}}}",
            self.cas_put.load(Relaxed),
            self.cas_put_new.load(Relaxed),
            self.cas_get_hit.load(Relaxed),
            self.cas_get_miss.load(Relaxed),
            self.kv_get_hit.load(Relaxed),
            self.kv_get_miss.load(Relaxed),
            self.kv_put.load(Relaxed),
            self.kv_put_refused.load(Relaxed),
            self.kv_put_conflict.load(Relaxed),
            self.bytes_in.load(Relaxed),
            self.bytes_out.load(Relaxed),
        )
    }
}

struct Store {
    root: PathBuf,
    read_only_kv: bool,
    stats: Stats,
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

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Store {
    fn cas_path(&self, id: &[u8]) -> PathBuf {
        let h = hex::encode(id);
        self.root.join("cas").join(&h[..2]).join(h)
    }

    fn kv_path(&self, key: &[u8]) -> PathBuf {
        let h = hex::encode(Sha256::digest(key));
        self.root.join("kv").join(&h[..2]).join(h)
    }

    fn bytes_of(b: Option<cas::CasBytes>) -> Result<Vec<u8>, Status> {
        match b.and_then(|b| b.contents) {
            None => Ok(Vec::new()),
            Some(cas::cas_bytes::Contents::Data(d)) => Ok(d),
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
            let obj = cas::CasObject {
                blob: Some(cas::CasBytes {
                    contents: Some(cas::cas_bytes::Contents::Data(data)),
                }),
                references: refs,
            };
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
        let data = match obj.blob.as_ref().and_then(|b| b.contents.as_ref()) {
            Some(cas::cas_bytes::Contents::Data(d)) => d.as_slice(),
            _ => &[],
        };
        object_id(&obj.references, data) == id
    }

    fn get(&self, id: &[u8]) -> Result<Option<cas::CasObject>, Status> {
        match std::fs::read(self.cas_path(id)) {
            Ok(bytes) => {
                // Corruption is a miss, never a wrong answer.
                let Ok(obj) = cas::CasObject::decode(bytes.as_slice()) else {
                    self.stats.cas_get_miss.fetch_add(1, Relaxed);
                    return Ok(None);
                };
                let data = match obj.blob.as_ref().and_then(|b| b.contents.as_ref()) {
                    Some(cas::cas_bytes::Contents::Data(d)) => d.as_slice(),
                    _ => &[],
                };
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

struct CasSvc(Arc<Store>);
struct KvSvc(Arc<Store>);

#[tonic::async_trait]
impl cas::casdb_service_server::CasdbService for CasSvc {
    async fn put(
        &self,
        r: Request<cas::CasPutRequest>,
    ) -> Result<Response<cas::CasPutResponse>, Status> {
        let obj = r.into_inner().data.unwrap_or_default();
        let data = Store::bytes_of(obj.blob)?;
        let id = self.0.put(obj.references, data)?;
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
        Ok(Response::new(match self.0.get(&id)? {
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
        let blob = r.into_inner().data.unwrap_or_default();
        let data = Store::bytes_of(blob.blob)?;
        let id = self.0.put(Vec::new(), data)?;
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
        Ok(Response::new(match self.0.get(&id)? {
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

#[tonic::async_trait]
impl kv::key_value_db_server::KeyValueDb for KvSvc {
    async fn get_value(
        &self,
        r: Request<kv::GetValueRequest>,
    ) -> Result<Response<kv::GetValueResponse>, Status> {
        let key = r.into_inner().key;
        let s = &self.0;
        match std::fs::read(s.kv_path(&key)) {
            Ok(bytes) => {
                let value = kv::Value::decode(bytes.as_slice())
                    .map_err(|e| Status::internal(e.to_string()))?;
                s.stats.kv_get_hit.fetch_add(1, Relaxed);
                Ok(Response::new(kv::GetValueResponse {
                    outcome: kv::get_value_response::Outcome::Success as i32,
                    contents: Some(kv::get_value_response::Contents::Value(value)),
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                s.stats.kv_get_miss.fetch_add(1, Relaxed);
                Ok(Response::new(kv::GetValueResponse {
                    outcome: kv::get_value_response::Outcome::KeyNotFound as i32,
                    contents: None,
                }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn put_value(
        &self,
        r: Request<kv::PutValueRequest>,
    ) -> Result<Response<kv::PutValueResponse>, Status> {
        let s = &self.0;
        if s.read_only_kv {
            s.stats.kv_put_refused.fetch_add(1, Relaxed);
            return Ok(Response::new(kv::PutValueResponse {
                error: Some(kv::ResponseError {
                    description: "read-only cache client".into(),
                }),
            }));
        }
        let req = r.into_inner();
        let bytes = req.value.unwrap_or_default().encode_to_vec();
        let path = s.kv_path(&req.key);
        s.stats.kv_put.fetch_add(1, Relaxed);
        // First writer wins: an existing mapping is never replaced.
        match std::fs::read(&path) {
            Ok(existing) => {
                if existing != bytes {
                    s.stats.kv_put_conflict.fetch_add(1, Relaxed);
                }
            }
            Err(_) => {
                std::fs::create_dir_all(path.parent().unwrap())
                    .and_then(|_| write_atomic(&path, &bytes))
                    .map_err(|e| Status::internal(e.to_string()))?;
            }
        }
        Ok(Response::new(kv::PutValueResponse { error: None }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket = PathBuf::from(
        args.next()
            .expect("usage: fleet-cas <socket> <store> [--read-only-kv]"),
    );
    let root = PathBuf::from(args.next().expect("store dir"));
    let read_only_kv = args.any(|a| a == "--read-only-kv");
    std::fs::create_dir_all(&root)?;
    let _ = std::fs::remove_file(&socket);
    let store = Arc::new(Store {
        root: root.clone(),
        read_only_kv,
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

    let listener = UnixListener::bind(&socket)?;
    eprintln!(
        "fleet-cas listening on {} store={} read_only_kv={read_only_kv}",
        socket.display(),
        root.display()
    );
    let final_store = store.clone();
    tonic::transport::Server::builder()
        .add_service(
            cas::casdb_service_server::CasdbServiceServer::new(CasSvc(store.clone()))
                .max_decoding_message_size(512 << 20)
                .max_encoding_message_size(512 << 20),
        )
        .add_service(kv::key_value_db_server::KeyValueDbServer::new(KvSvc(store)))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    eprintln!("{}", final_store.stats.line());
    Ok(())
}
