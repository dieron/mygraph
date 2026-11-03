//! Request statistics and the Prometheus text for `/metrics` (spec/api/admin.md [ADMIN-SRV-1],
//! [ADMIN-SRV-2], [ADMIN-MET-1]).

use crate::registry::GraphRun;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Upper bounds of the latency histogram, in seconds.
const BUCKETS: [f64; 12] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0,
];

/// [ADMIN-SRV-2] How many slow requests are kept.
const SLOW_KEPT: usize = 50;

#[derive(Default)]
struct OpStats {
    count: u64,
    errors: u64,
    slow: u64,
    /// By status class: "2xx", "4xx", "5xx".
    by_class: BTreeMap<&'static str, u64>,
    sum_s: f64,
    max_s: f64,
    buckets: [u64; BUCKETS.len()],
}

#[derive(Default)]
struct Inner {
    ops: BTreeMap<String, OpStats>,
    errors: BTreeMap<String, u64>,
    /// Newest first.
    slow: VecDeque<Json>,
}

#[derive(Default)]
pub struct Metrics {
    inner: Mutex<Inner>,
}

/// What `/metrics` reports about one graph ([ADMIN-MET-1]).
pub struct GraphMetrics {
    pub name: String,
    pub state: &'static str,
    pub file_bytes: (u64, u64),
    pub run: GraphRun,
    /// Present while the graph is loaded (and not busy with a long write).
    pub loaded: Option<LoadedMetrics>,
}

pub struct LoadedMetrics {
    pub counts: [(&'static str, usize); 4],
    pub last_seq: i64,
    /// (pending updates, stale) when the graph has a full-text index.
    pub text: Option<(usize, bool)>,
    /// Documents waiting to be embedded, when the graph has a semantic index.
    pub semantic_pending: Option<usize>,
}

/// Process-wide values for `/metrics`.
pub struct ProcessMetrics {
    pub version: &'static str,
    pub spec: &'static str,
    pub storage_format: u32,
    pub uptime_s: f64,
    pub resident_bytes: Option<u64>,
}

/// The `# HELP` and `# TYPE` lines of a metric.
fn head(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

fn class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

impl Metrics {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one request: `op` as in [ADMIN-SRV-1], the error `code` of an error answer, and
    /// whether it was slow ([ADMIN-SRV-2]).
    pub fn record(
        &self,
        op: &str,
        graph: Option<&str>,
        status: u16,
        took: Duration,
        code: Option<&str>,
        slow: bool,
    ) {
        let s = took.as_secs_f64();
        let mut inner = self.lock();
        let o = match inner.ops.get_mut(op) {
            Some(o) => o,
            None => inner.ops.entry(op.to_owned()).or_default(),
        };
        o.count += 1;
        if status >= 400 {
            o.errors += 1;
        }
        *o.by_class.entry(class(status)).or_default() += 1;
        o.sum_s += s;
        o.max_s = o.max_s.max(s);
        for (b, le) in o.buckets.iter_mut().zip(BUCKETS) {
            if s <= le {
                *b += 1;
            }
        }
        if slow {
            o.slow += 1;
            inner.slow.push_front(json!({
                "at": mygraph_core::model::time_json(crate::registry::now_ms()), "op": op,
                "graph": graph, "status": status, "ms": (s * 1e5).round() / 100.0,
            }));
            inner.slow.truncate(SLOW_KEPT);
        }
        if let Some(c) = code {
            *inner.errors.entry(c.to_owned()).or_default() += 1;
        }
    }

    /// [ADMIN-SRV-1]
    pub fn summary(&self) -> Json {
        let inner = self.lock();
        let m: Map<String, Json> = inner
            .ops
            .iter()
            .map(|(k, o)| {
                let mean = if o.count == 0 {
                    0.0
                } else {
                    o.sum_s / o.count as f64
                };
                (
                    k.clone(),
                    json!({
                        "count": o.count, "errors": o.errors,
                        "mean_ms": (mean * 1e5).round() / 100.0,
                        "max_ms": (o.max_s * 1e5).round() / 100.0,
                    }),
                )
            })
            .collect();
        Json::Object(m)
    }

    /// [ADMIN-SRV-2] Recent slow requests, newest first.
    pub fn slow_ops(&self) -> Vec<Json> {
        self.lock().slow.iter().cloned().collect()
    }

    /// Prometheus text exposition format, version 0.0.4 ([ADMIN-MET-1]).
    pub fn prometheus(&self, p: &ProcessMetrics, graphs: &[GraphMetrics]) -> String {
        let inner = self.lock();
        let mut out = String::new();
        head(
            &mut out,
            "mygraph_build_info",
            "gauge",
            "Server, spec and storage format versions.",
        );
        let _ = writeln!(
            out,
            "mygraph_build_info{{version=\"{}\",spec=\"{}\",storage_format=\"{}\"}} 1",
            p.version, p.spec, p.storage_format
        );
        head(
            &mut out,
            "mygraph_uptime_seconds",
            "gauge",
            "Seconds since the server started.",
        );
        let _ = writeln!(out, "mygraph_uptime_seconds {:.3}", p.uptime_s);
        if let Some(b) = p.resident_bytes {
            head(
                &mut out,
                "mygraph_process_resident_bytes",
                "gauge",
                "Resident memory.",
            );
            let _ = writeln!(out, "mygraph_process_resident_bytes {b}");
        }

        head(
            &mut out,
            "mygraph_requests_total",
            "counter",
            "Requests by operation and status class.",
        );
        for (op, o) in &inner.ops {
            for (class, n) in &o.by_class {
                let _ = writeln!(
                    out,
                    "mygraph_requests_total{{op=\"{op}\",status=\"{class}\"}} {n}"
                );
            }
        }
        head(
            &mut out,
            "mygraph_request_duration_seconds",
            "histogram",
            "Request latency by operation.",
        );
        for (op, o) in &inner.ops {
            let name = "mygraph_request_duration_seconds";
            for (n, le) in o.buckets.iter().zip(BUCKETS) {
                let _ = writeln!(out, "{name}_bucket{{op=\"{op}\",le=\"{le}\"}} {n}");
            }
            let _ = writeln!(out, "{name}_bucket{{op=\"{op}\",le=\"+Inf\"}} {}", o.count);
            let _ = writeln!(out, "{name}_sum{{op=\"{op}\"}} {}", o.sum_s);
            let _ = writeln!(out, "{name}_count{{op=\"{op}\"}} {}", o.count);
        }
        head(
            &mut out,
            "mygraph_errors_total",
            "counter",
            "Error answers by error code.",
        );
        for (code, n) in &inner.errors {
            let _ = writeln!(out, "mygraph_errors_total{{code=\"{code}\"}} {n}");
        }
        head(
            &mut out,
            "mygraph_slow_requests_total",
            "counter",
            "Requests slower than slow_ms.",
        );
        for (op, o) in inner.ops.iter().filter(|(_, o)| o.slow > 0) {
            let _ = writeln!(out, "mygraph_slow_requests_total{{op=\"{op}\"}} {}", o.slow);
        }

        head(
            &mut out,
            "mygraph_graph_state",
            "gauge",
            "1 for the graph's current state.",
        );
        for g in graphs {
            let _ = writeln!(
                out,
                "mygraph_graph_state{{graph=\"{}\",state=\"{}\"}} 1",
                g.name, g.state
            );
        }
        head(
            &mut out,
            "mygraph_graph_load_seconds",
            "gauge",
            "Duration of the graph's last load.",
        );
        for g in graphs {
            if let Some(s) = g.run.load_s {
                let _ = writeln!(
                    out,
                    "mygraph_graph_load_seconds{{graph=\"{}\"}} {s:.3}",
                    g.name
                );
            }
        }
        head(
            &mut out,
            "mygraph_graph_file_bytes",
            "gauge",
            "Database and write-ahead log sizes.",
        );
        for g in graphs {
            let (db, wal) = g.file_bytes;
            let _ = writeln!(
                out,
                "mygraph_graph_file_bytes{{graph=\"{}\",file=\"db\"}} {db}",
                g.name
            );
            let _ = writeln!(
                out,
                "mygraph_graph_file_bytes{{graph=\"{}\",file=\"wal\"}} {wal}",
                g.name
            );
        }
        let loaded = || {
            graphs
                .iter()
                .filter_map(|g| g.loaded.as_ref().map(|l| (&g.name, l)))
        };
        head(
            &mut out,
            "mygraph_objects",
            "gauge",
            "Active objects per graph and kind.",
        );
        for (g, l) in loaded() {
            for (kind, n) in l.counts {
                let _ = writeln!(out, "mygraph_objects{{graph=\"{g}\",kind=\"{kind}\"}} {n}");
            }
        }
        head(
            &mut out,
            "mygraph_graph_last_seq",
            "gauge",
            "Latest change-log sequence number.",
        );
        for (g, l) in loaded() {
            let _ = writeln!(
                out,
                "mygraph_graph_last_seq{{graph=\"{g}\"}} {}",
                l.last_seq
            );
        }
        head(
            &mut out,
            "mygraph_text_index_pending",
            "gauge",
            "Updates not yet committed to the full-text index.",
        );
        for (g, l) in loaded() {
            if let Some((pending, _)) = l.text {
                let _ = writeln!(out, "mygraph_text_index_pending{{graph=\"{g}\"}} {pending}");
            }
        }
        head(
            &mut out,
            "mygraph_text_index_stale",
            "gauge",
            "1 when search fell back to the store's full text.",
        );
        for (g, l) in loaded() {
            if let Some((_, stale)) = l.text {
                let _ = writeln!(
                    out,
                    "mygraph_text_index_stale{{graph=\"{g}\"}} {}",
                    u8::from(stale)
                );
            }
        }
        head(
            &mut out,
            "mygraph_text_commits_total",
            "counter",
            "Background full-text commits.",
        );
        for g in graphs {
            let r = &g.run;
            let _ = writeln!(
                out,
                "mygraph_text_commits_total{{graph=\"{}\",outcome=\"ok\"}} {}",
                g.name, r.text_commits
            );
            let _ = writeln!(
                out,
                "mygraph_text_commits_total{{graph=\"{}\",outcome=\"error\"}} {}",
                g.name, r.text_commit_errors
            );
        }
        head(
            &mut out,
            "mygraph_text_commit_seconds_total",
            "counter",
            "Time spent in full-text commits.",
        );
        for g in graphs {
            let _ = writeln!(
                out,
                "mygraph_text_commit_seconds_total{{graph=\"{}\"}} {}",
                g.name, g.run.text_commit_s
            );
        }
        head(
            &mut out,
            "mygraph_semantic_index_pending",
            "gauge",
            "Documents waiting to be embedded into the semantic index.",
        );
        for (g, l) in loaded() {
            if let Some(pending) = l.semantic_pending {
                let _ = writeln!(
                    out,
                    "mygraph_semantic_index_pending{{graph=\"{g}\"}} {pending}"
                );
            }
        }
        head(
            &mut out,
            "mygraph_semantic_commits_total",
            "counter",
            "Background semantic index commits (embedding and saving).",
        );
        for g in graphs {
            let r = &g.run;
            let _ = writeln!(
                out,
                "mygraph_semantic_commits_total{{graph=\"{}\",outcome=\"ok\"}} {}",
                g.name, r.semantic_commits
            );
            let _ = writeln!(
                out,
                "mygraph_semantic_commits_total{{graph=\"{}\",outcome=\"error\"}} {}",
                g.name, r.semantic_commit_errors
            );
        }
        head(
            &mut out,
            "mygraph_semantic_commit_seconds_total",
            "counter",
            "Time spent in semantic index commits.",
        );
        for g in graphs {
            let _ = writeln!(
                out,
                "mygraph_semantic_commit_seconds_total{{graph=\"{}\"}} {}",
                g.name, g.run.semantic_commit_s
            );
        }
        head(
            &mut out,
            "mygraph_checkpoints_total",
            "counter",
            "Background WAL checkpoints.",
        );
        for g in graphs {
            let r = &g.run;
            let _ = writeln!(
                out,
                "mygraph_checkpoints_total{{graph=\"{}\",outcome=\"ok\"}} {}",
                g.name, r.checkpoints
            );
            let _ = writeln!(
                out,
                "mygraph_checkpoints_total{{graph=\"{}\",outcome=\"error\"}} {}",
                g.name, r.checkpoint_errors
            );
        }
        head(
            &mut out,
            "mygraph_checkpoint_seconds_total",
            "counter",
            "Time spent in WAL checkpoints.",
        );
        for g in graphs {
            let _ = writeln!(
                out,
                "mygraph_checkpoint_seconds_total{{graph=\"{}\"}} {}",
                g.name, g.run.checkpoint_s
            );
        }
        out
    }
}
