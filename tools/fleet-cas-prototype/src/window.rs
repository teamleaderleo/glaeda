//! Request counters over time windows, kept across restarts, and the JSON
//! that `GET /stats` serves.
//!
//! Why: the fleet store on cmux-lawrence was down ~21 h (2026-11-20/21) and
//! nothing said so. Its counters lived only in the process (`stats.json`,
//! reset at every start) and nothing on the network could read them, so a
//! dead store looked like ordinary compiles on the nodes. Now the store keeps
//! per-minute buckets of hits, misses, puts and errors for 24 h in
//! `stats-history.txt` beside its objects, sums them into 1 h and 24 h
//! windows, and serves them on its own port. A scraper reads `/stats`: no
//! answer is a down store, a low 1 h hit rate with enough requests is a cold
//! or broken one.
//!
//! The process counters (`Stats`) stay the source: every sample turns their
//! growth since the last sample into the current minute's bucket, so the hot
//! paths change nothing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// The windowed counters, in bucket order.
pub const KEYS: [&str; 9] = [
    "kv_hits",
    "kv_misses",
    "cas_hits",
    "cas_misses",
    "puts",
    "errors",
    "write_refused",
    "bytes_in",
    "bytes_out",
];
pub const N: usize = KEYS.len();
const KV_HITS: usize = 0;
const KV_MISSES: usize = 1;
const CAS_HITS: usize = 2;
const CAS_MISSES: usize = 3;

/// 24 h of minute buckets.
pub const KEEP_MIN: u64 = 24 * 60;
pub const HISTORY_FILE: &str = "stats-history.txt";
const HEADER: &str = "fleet-cas-history v1";

pub type Counts = [u64; N];

#[derive(Debug)]
pub struct History {
    /// (unix minute, counts in that minute), oldest first, within KEEP_MIN.
    buckets: VecDeque<(u64, Counts)>,
    /// The process counters at the last sample.
    last: Counts,
    /// Every count since the history file was first written.
    pub lifetime: Counts,
    /// When the history file was first written (unix ms), and how many
    /// processes have started on it.
    pub first_started_ms: u64,
    pub starts: u64,
    path: Option<PathBuf>,
}

fn add(a: &mut Counts, b: &Counts) {
    for (x, y) in a.iter_mut().zip(b) {
        *x = x.saturating_add(*y);
    }
}

impl History {
    /// A history with nothing in it, kept in memory only.
    pub fn new(now_ms: u64) -> Self {
        History {
            buckets: VecDeque::new(),
            last: [0; N],
            lifetime: [0; N],
            first_started_ms: now_ms,
            starts: 1,
            path: None,
        }
    }

    /// Load `dir/stats-history.txt` for a process that just started (its
    /// counters are all 0). A missing or unreadable file starts a new
    /// history; buckets older than KEEP_MIN are dropped.
    pub fn load(dir: &Path, now_ms: u64) -> Self {
        let path = dir.join(HISTORY_FILE);
        let mut h = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| Self::parse(&text))
        {
            // One more start on a kept history.
            Some(h) => History {
                starts: h.starts + 1,
                ..h
            },
            None => Self::new(now_ms),
        };
        h.path = Some(path);
        h.prune(now_ms / 60_000);
        h
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let mut h = Self::new(0);
        h.starts = 0;
        // Column i of the file is counter cols[i] here (None: a key this
        // build does not keep).
        let mut cols: Vec<Option<usize>> = Vec::new();
        let row = |cols: &[Option<usize>], fields: std::str::SplitWhitespace| -> Option<Counts> {
            let mut c = [0; N];
            for (i, f) in fields.enumerate() {
                let v: u64 = f.parse().ok()?;
                if let Some(Some(k)) = cols.get(i) {
                    c[*k] = v;
                }
            }
            Some(c)
        };
        for line in lines {
            let mut f = line.split_whitespace();
            match f.next() {
                Some("started") => {
                    h.first_started_ms = f.next()?.parse().ok()?;
                    h.starts = f.next()?.parse().ok()?;
                }
                Some("keys") => cols = f.map(|k| KEYS.iter().position(|x| *x == k)).collect(),
                Some("lifetime") => h.lifetime = row(&cols, f)?,
                Some("m") => {
                    let minute: u64 = f.next()?.parse().ok()?;
                    let c = row(&cols, f)?;
                    // Keep the order the file was written in; ignore a row
                    // that goes back in time.
                    if h.buckets.back().is_none_or(|(m, _)| *m < minute) {
                        h.buckets.push_back((minute, c));
                    }
                }
                _ => {}
            }
        }
        Some(h)
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "{HEADER}\nstarted {} {}\nkeys {}\n",
            self.first_started_ms,
            self.starts,
            KEYS.join(" ")
        );
        let nums = |c: &Counts| c.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        out.push_str(&format!("lifetime {}\n", nums(&self.lifetime)));
        for (m, c) in &self.buckets {
            out.push_str(&format!("m {m} {}\n", nums(c)));
        }
        out
    }

    /// Write the history file atomically (no-op for an in-memory history).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, self.render())?;
        std::fs::rename(&tmp, path)
    }

    fn prune(&mut self, now_min: u64) {
        while self
            .buckets
            .front()
            .is_some_and(|(m, _)| m + KEEP_MIN <= now_min)
        {
            self.buckets.pop_front();
        }
    }

    /// Fold the process counters' growth since the last sample into the
    /// current minute.
    pub fn sample(&mut self, current: &Counts, now_ms: u64) {
        let mut delta = [0; N];
        for i in 0..N {
            delta[i] = current[i].saturating_sub(self.last[i]);
        }
        self.last = *current;
        let minute = now_ms / 60_000;
        self.prune(minute);
        if delta.iter().all(|d| *d == 0) {
            return;
        }
        add(&mut self.lifetime, &delta);
        match self.buckets.back_mut() {
            Some((m, c)) if *m >= minute => add(c, &delta),
            _ => self.buckets.push_back((minute, delta)),
        }
    }

    /// Counts in the last `minutes` minutes (the current minute included),
    /// with the growth not sampled yet.
    pub fn window(&self, current: &Counts, now_ms: u64, minutes: u64) -> Counts {
        let now_min = now_ms / 60_000;
        let mut out = [0; N];
        for (m, c) in self.buckets.iter().rev() {
            if m + minutes <= now_min {
                break;
            }
            add(&mut out, c);
        }
        for i in 0..N {
            out[i] = out[i].saturating_add(current[i].saturating_sub(self.last[i]));
        }
        out
    }

    /// Like `window`, for the lifetime counts.
    pub fn lifetime_with(&self, current: &Counts) -> Counts {
        let mut out = self.lifetime;
        for i in 0..N {
            out[i] = out[i].saturating_add(current[i].saturating_sub(self.last[i]));
        }
        out
    }
}

fn rate(hits: u64, misses: u64) -> String {
    match hits + misses {
        0 => "null".into(),
        n => format!("{:.4}", hits as f64 / n as f64),
    }
}

/// One window as a JSON object: the counts, `requests` (lookups: index and
/// object gets), `hit_rate` over all lookups and `kv_hit_rate` over index
/// lookups (a compile replayed or not). Rates are null with no lookups.
pub fn counts_json(c: &Counts) -> String {
    let mut fields: Vec<String> = KEYS
        .iter()
        .zip(c)
        .map(|(k, v)| format!("\"{k}\":{v}"))
        .collect();
    let hits = c[KV_HITS] + c[CAS_HITS];
    let misses = c[KV_MISSES] + c[CAS_MISSES];
    fields.push(format!("\"requests\":{}", hits + misses));
    fields.push(format!("\"hits\":{hits}"));
    fields.push(format!("\"misses\":{misses}"));
    fields.push(format!("\"hit_rate\":{}", rate(hits, misses)));
    fields.push(format!(
        "\"kv_hit_rate\":{}",
        rate(c[KV_HITS], c[KV_MISSES])
    ));
    format!("{{{}}}", fields.join(","))
}

/// The windows `/stats` reports, as (name, minutes).
pub const WINDOWS: [(&str, u64); 3] = [("5m", 5), ("1h", 60), ("24h", KEEP_MIN)];

pub fn windows_json(h: &History, current: &Counts, now_ms: u64) -> String {
    let body: Vec<String> = WINDOWS
        .iter()
        .map(|(name, minutes)| {
            format!(
                "\"{name}\":{}",
                counts_json(&h.window(current, now_ms, *minutes))
            )
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

/// A JSON string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    fn counts(kv_hits: u64, kv_misses: u64, puts: u64, errors: u64) -> Counts {
        let mut c = [0; N];
        c[0] = kv_hits;
        c[1] = kv_misses;
        c[4] = puts;
        c[5] = errors;
        c
    }

    #[test]
    fn samples_fold_into_minutes_and_windows_sum_them() {
        let t0 = 1_000 * MIN;
        let mut h = History::new(t0);
        h.sample(&counts(3, 1, 0, 0), t0);
        h.sample(&counts(5, 1, 2, 0), t0 + 10_000); // same minute
        h.sample(&counts(5, 4, 2, 1), t0 + 2 * MIN);
        assert_eq!(h.buckets.len(), 2);
        let now = t0 + 2 * MIN + 1;
        assert_eq!(h.window(&counts(5, 4, 2, 1), now, 1), counts(0, 3, 0, 1));
        assert_eq!(h.window(&counts(5, 4, 2, 1), now, 60), counts(5, 4, 2, 1));
        // Not sampled yet still counts.
        assert_eq!(h.window(&counts(6, 4, 2, 1), now, 60), counts(6, 4, 2, 1));
        // An hour later the first minutes are out of the 1 h window, in the 24 h one.
        let later = t0 + 61 * MIN;
        assert_eq!(h.window(&counts(5, 4, 2, 1), later, 60), counts(0, 3, 0, 1));
        assert_eq!(
            h.window(&counts(5, 4, 2, 1), later, KEEP_MIN),
            counts(5, 4, 2, 1)
        );
        // Past 24 h the buckets are dropped at the next sample; lifetime stays.
        h.sample(&counts(5, 4, 2, 1), t0 + (KEEP_MIN + 3) * MIN);
        assert!(h.buckets.is_empty());
        assert_eq!(h.lifetime, counts(5, 4, 2, 1));
    }

    #[test]
    fn history_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("fleet-cas-window-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t0 = 2_000 * MIN;
        let mut h = History::load(&dir, t0);
        assert_eq!(h.starts, 1);
        h.sample(&counts(7, 3, 1, 0), t0 + 5_000);
        h.save().unwrap();

        // The new process's counters start at 0 again.
        let t1 = t0 + 10 * MIN;
        let mut h2 = History::load(&dir, t1);
        assert_eq!(h2.starts, 2);
        assert_eq!(h2.first_started_ms, t0);
        assert_eq!(h2.lifetime, counts(7, 3, 1, 0));
        assert_eq!(h2.window(&[0; N], t1, 60), counts(7, 3, 1, 0));
        h2.sample(&counts(1, 0, 0, 2), t1);
        assert_eq!(h2.window(&counts(1, 0, 0, 2), t1, 60), counts(8, 3, 1, 2));
        assert_eq!(h2.lifetime, counts(8, 3, 1, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_or_foreign_history_starts_over() {
        assert!(History::parse("something else\n").is_none());
        assert!(History::parse(&format!("{HEADER}\nkeys kv_hits\nm x 1\n")).is_none());
        // Unknown keys are skipped, known ones land by name.
        let h = History::parse(&format!(
            "{HEADER}\nstarted 5 2\nkeys future kv_misses\nlifetime 9 4\nm 10 1 2\n"
        ))
        .unwrap();
        assert_eq!(h.lifetime[KV_MISSES], 4);
        assert_eq!(h.buckets[0].1[KV_MISSES], 2);
        assert_eq!(h.starts, 2);
    }

    #[test]
    fn window_json_has_rates_and_null_without_lookups() {
        let j = counts_json(&counts(3, 1, 0, 0));
        assert!(j.contains("\"requests\":4"), "{j}");
        assert!(j.contains("\"kv_hit_rate\":0.7500"), "{j}");
        assert!(j.contains("\"hit_rate\":0.7500"), "{j}");
        let j = counts_json(&[0; N]);
        assert!(j.contains("\"hit_rate\":null"), "{j}");
        assert_eq!(quote("a\"b\\\n"), "\"a\\\"b\\\\\\u000a\"");
    }
}
