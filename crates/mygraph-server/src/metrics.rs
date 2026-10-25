//! Request statistics: Prometheus text for `/metrics`, a summary for `/admin/stats`.

use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Upper bounds of the latency histogram, in seconds.
const BUCKETS: [f64; 12] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0,
];

#[derive(Default)]
struct OpStats {
    count: u64,
    errors: u64,
    /// By status class: "2xx", "4xx", "5xx".
    by_class: BTreeMap<&'static str, u64>,
    sum_s: f64,
    max_s: f64,
    buckets: [u64; BUCKETS.len()],
}

#[derive(Default)]
pub struct Metrics {
    ops: Mutex<BTreeMap<String, OpStats>>,
}

impl Metrics {
    pub fn record(&self, op: &str, status: u16, took: Duration) {
        let s = took.as_secs_f64();
        let mut ops = self.ops.lock().unwrap_or_else(PoisonError::into_inner);
        let o = match ops.get_mut(op) {
            Some(o) => o,
            None => ops.entry(op.to_owned()).or_default(),
        };
        o.count += 1;
        if status >= 400 {
            o.errors += 1;
        }
        let class = match status {
            200..=299 => "2xx",
            300..=399 => "3xx",
            400..=499 => "4xx",
            _ => "5xx",
        };
        *o.by_class.entry(class).or_default() += 1;
        o.sum_s += s;
        o.max_s = o.max_s.max(s);
        for (b, le) in o.buckets.iter_mut().zip(BUCKETS) {
            if s <= le {
                *b += 1;
            }
        }
    }

    /// [ADMIN-SRV-1]
    pub fn summary(&self) -> Json {
        let ops = self.ops.lock().unwrap_or_else(PoisonError::into_inner);
        let m: Map<String, Json> = ops
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

    /// Prometheus text exposition format; `graphs` adds object counts per loaded graph.
    pub fn prometheus(&self, graphs: &[(String, [(&'static str, usize); 4])]) -> String {
        let ops = self.ops.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = String::new();
        out.push_str("# HELP mygraph_requests_total Requests by operation and status class.\n");
        out.push_str("# TYPE mygraph_requests_total counter\n");
        for (op, o) in ops.iter() {
            for (class, n) in &o.by_class {
                let _ = writeln!(
                    out,
                    "mygraph_requests_total{{op=\"{op}\",status=\"{class}\"}} {n}"
                );
            }
        }
        out.push_str("# HELP mygraph_request_duration_seconds Request latency by operation.\n");
        out.push_str("# TYPE mygraph_request_duration_seconds histogram\n");
        for (op, o) in ops.iter() {
            for (n, le) in o.buckets.iter().zip(BUCKETS) {
                let _ = writeln!(
                    out,
                    "mygraph_request_duration_seconds_bucket{{op=\"{op}\",le=\"{le}\"}} {n}"
                );
            }
            let _ = writeln!(
                out,
                "mygraph_request_duration_seconds_bucket{{op=\"{op}\",le=\"+Inf\"}} {}",
                o.count
            );
            let _ = writeln!(
                out,
                "mygraph_request_duration_seconds_sum{{op=\"{op}\"}} {}",
                o.sum_s
            );
            let _ = writeln!(
                out,
                "mygraph_request_duration_seconds_count{{op=\"{op}\"}} {}",
                o.count
            );
        }
        out.push_str("# HELP mygraph_objects Active objects per graph and kind.\n");
        out.push_str("# TYPE mygraph_objects gauge\n");
        for (g, counts) in graphs {
            for (kind, n) in counts {
                let _ = writeln!(out, "mygraph_objects{{graph=\"{g}\",kind=\"{kind}\"}} {n}");
            }
        }
        out
    }
}
