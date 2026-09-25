//! `fleet-cas warm`: fill a node store with a whole commit before a build.
//!
//! A node answers each of Xcode's index lookups it lacks with a fetch from
//! the fleet store, then a closure prefetch, before Xcode can go on. Xcode
//! issues those lookups mostly one at a time, so a catch-up build that reads
//! everything remotely costs thousands of serial round trips (the 09-25 pair
//! test: 7837 lookups, about 336 ms each under load, 1631 s). The writer knows
//! every key its fill used: `fleet-cas-writer-build.sh` stores that list as a
//! CAS object (the manifest) and names it in the signed marker. `warm` reads
//! the marker, fetches the listed entries with many calls in flight, streams
//! the objects they reach in a few closure calls, and writes both into the
//! node's store, so the build's lookups are local hits.
//!
//! Nothing is trusted that the node would not trust: the marker and every
//! entry must carry a trusted signature, the manifest and every object are
//! checked against their content IDs.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use prost::Message;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{
    MANIFEST_ENTRY, MAX_CLOSURE_ROOTS, Stats, Store, USAGE, blob_data, cas, embedded_ids, fleet,
    kv, object_id, publish_new, sign, write_atomic,
};

/// The manifest: one hex index key per line. Refuses an empty or malformed list.
pub fn parse_manifest(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let text = std::str::from_utf8(data).map_err(|_| "manifest is not text")?;
    let mut keys = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        keys.push(hex::decode(line).map_err(|_| format!("manifest line is not hex: {line:.40}"))?);
    }
    if keys.is_empty() {
        return Err("manifest lists no keys".into());
    }
    Ok(keys)
}

enum Entry {
    Local(Vec<Vec<u8>>),
    Fetched(Vec<Vec<u8>>),
    Missing,
    Unverified,
}

pub async fn run(args: &[String]) -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    let (Some(url), Some(name)) = (args.first(), args.get(1)) else {
        return Err(USAGE.into());
    };
    let mut trusted = None;
    let mut dir = None;
    let mut jobs = 32usize;
    let mut rest = args[2..].iter();
    while let Some(a) = rest.next() {
        let v = rest.next().ok_or(USAGE)?;
        match a.as_str() {
            "--trusted-keys" => trusted = Some(sign::parse_trusted(v)?),
            "--store" => dir = Some(PathBuf::from(v)),
            "--jobs" => jobs = v.parse::<usize>()?.clamp(1, 256),
            _ => return Err(USAGE.into()),
        }
    }
    let keys = trusted.ok_or("warm needs --trusted-keys")?;
    let dir = dir.ok_or("warm needs --store DIR (the node's store)")?;
    let t0 = Instant::now();

    let ch = tonic::transport::Endpoint::from_shared(url.clone())?
        .tcp_nodelay(true)
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(60))
        .connect()
        .await?;
    let kv_client = kv::key_value_db_client::KeyValueDbClient::new(ch.clone());
    let mut cas_client = cas::casdb_service_client::CasdbServiceClient::new(ch.clone())
        .max_decoding_message_size(512 << 20);
    let fleet_client =
        fleet::fleet_cas_client::FleetCasClient::new(ch).max_decoding_message_size(512 << 20);

    // 1. The marker, and the manifest it names.
    let mkey = sign::marker_key(name);
    let resp = kv_client
        .clone()
        .get_value(kv::GetValueRequest { key: mkey.clone() })
        .await?;
    let Some(kv::get_value_response::Contents::Value(marker)) = resp.into_inner().contents else {
        eprintln!("fleet-cas warm: no marker {name}");
        return Ok(std::process::ExitCode::from(1));
    };
    if !sign::verify(&keys, &mkey, &marker) {
        eprintln!("fleet-cas warm: marker {name} is not signed by a trusted key");
        return Ok(std::process::ExitCode::from(2));
    }
    let Some(mid) = marker.entries.get(MANIFEST_ENTRY).cloned() else {
        eprintln!("fleet-cas warm: marker {name} has no manifest (written before warm existed)");
        return Ok(std::process::ExitCode::from(1));
    };
    let resp = cas_client
        .get(cas::CasGetRequest {
            cas_id: Some(cas::CasDataId { id: mid.clone() }),
            write_to_disk: false,
        })
        .await?
        .into_inner();
    let Some(cas::cas_get_response::Contents::Data(obj)) = resp.contents else {
        return Err(format!("the store lacks manifest {}", hex::encode(&mid)).into());
    };
    if object_id(&obj.references, blob_data(&obj)) != mid {
        return Err("the manifest does not match its ID".into());
    }
    let wanted = parse_manifest(blob_data(&obj))?;

    // The node's store, written the way the node writes it.
    std::fs::create_dir_all(&dir)?;
    let store = Arc::new(Store {
        root: dir,
        read_only_kv: true,
        writers: None,
        allow_file_paths: false,
        upstream: None,
        sign_key: None,
        trusted: Some(keys),
        strip_signatures: true,
        upstream_down_until: AtomicU64::new(0),
        stats: Stats::default(),
        key_log: None,
    });

    // 2. Index entries, many in flight.
    let slots = Arc::new(Semaphore::new(jobs));
    let mut set = JoinSet::new();
    for key in wanted.iter().cloned() {
        let store = store.clone();
        let mut client = kv_client.clone();
        let permit = slots.clone().acquire_owned().await?;
        set.spawn(async move {
            let _permit = permit;
            let path = store.kv_path(&key);
            let mut replace = false;
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(v) = kv::Value::decode(bytes.as_slice()) {
                    if store.trusts(&key, &v) {
                        return Ok(Entry::Local(embedded_ids(&v)));
                    }
                }
                replace = true;
            }
            let resp = client
                .get_value(kv::GetValueRequest { key: key.clone() })
                .await
                .map_err(|e| e.to_string())?;
            let Some(kv::get_value_response::Contents::Value(v)) = resp.into_inner().contents else {
                return Ok(Entry::Missing);
            };
            if !store.trusts(&key, &v) {
                return Ok(Entry::Unverified);
            }
            let bytes = v.encode_to_vec();
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            if replace {
                write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
            } else {
                // Lost a race with the node: its copy is as good.
                publish_new(&path, &bytes).map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(Entry::Fetched(embedded_ids(&v)))
        });
    }
    let (mut local, mut fetched, mut missing, mut unverified, mut errors) = (0, 0, 0, 0, 0);
    let mut roots: HashSet<Vec<u8>> = HashSet::new();
    let mut first_error = None;
    while let Some(r) = set.join_next().await {
        match r? {
            Ok(Entry::Local(ids)) => {
                local += 1;
                roots.extend(ids);
            }
            Ok(Entry::Fetched(ids)) => {
                fetched += 1;
                roots.extend(ids);
            }
            Ok(Entry::Missing) => missing += 1,
            Ok(Entry::Unverified) => unverified += 1,
            Err(e) => {
                errors += 1;
                first_error.get_or_insert(e);
            }
        }
    }

    // 3. Everything those entries reach, in a few streamed closure calls.
    let mut roots: Vec<Vec<u8>> = roots
        .into_iter()
        .filter(|id| !store.cas_path(id).exists())
        .collect();
    roots.sort();
    let objects = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let streams = Arc::new(Semaphore::new(4));
    let mut set = JoinSet::new();
    for chunk in roots.chunks(MAX_CLOSURE_ROOTS) {
        let chunk = chunk.to_vec();
        let store = store.clone();
        let mut client = fleet_client.clone();
        let (objects, bytes) = (objects.clone(), bytes.clone());
        let permit = streams.clone().acquire_owned().await?;
        set.spawn(async move {
            let _permit = permit;
            // The store bounds closure walks in flight; a busy store is retried.
            let mut tries = 0;
            let mut stream = loop {
                match client
                    .get_closure(fleet::ClosureRequest { roots: chunk.clone() })
                    .await
                {
                    Ok(r) => break r.into_inner(),
                    Err(e) if e.code() == tonic::Code::ResourceExhausted && tries < 10 => {
                        tries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => return Err(e.to_string()),
                }
            };
            while let Some(m) = stream.message().await.map_err(|e| e.to_string())? {
                let Ok(obj) = cas::CasObject::decode(m.object.as_slice()) else {
                    return Err("undecodable object in a closure".into());
                };
                let data = blob_data(&obj).to_vec();
                if object_id(&obj.references, &data) != m.id {
                    return Err("closure object does not match its ID".into());
                }
                if store.cas_path(&m.id).exists() {
                    continue;
                }
                let len = data.len() as u64;
                store.put(obj.references, data).map_err(|e| e.message().to_string())?;
                objects.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                bytes.fetch_add(len, std::sync::atomic::Ordering::Relaxed);
            }
            Ok::<_, String>(())
        });
    }
    while let Some(r) = set.join_next().await {
        if let Err(e) = r? {
            errors += 1;
            first_error.get_or_insert(e);
        }
    }

    let load = std::sync::atomic::Ordering::Relaxed;
    println!(
        "warm {name}: {} keys ({local} local, {fetched} fetched, {missing} missing, \
         {unverified} unverified), {} objects ({} bytes) in {:.1} s",
        wanted.len(),
        objects.load(load),
        bytes.load(load),
        t0.elapsed().as_secs_f64()
    );
    if let Some(e) = first_error {
        eprintln!("fleet-cas warm: {errors} failed calls, first: {e}");
        return Ok(std::process::ExitCode::from(2));
    }
    if unverified > 0 {
        eprintln!("fleet-cas warm: {unverified} entries without a trusted signature");
        return Ok(std::process::ExitCode::from(2));
    }
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parsing() {
        assert_eq!(parse_manifest(b"00ff\nab\n").unwrap(), vec![vec![0, 255], vec![0xab]]);
        assert!(parse_manifest(b"").is_err());
        assert!(parse_manifest(b"\n\n").is_err());
        assert!(parse_manifest(b"zz\n").is_err());
    }
}
