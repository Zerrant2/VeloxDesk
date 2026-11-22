//! Latency statistics and report formatting for benches.

use veloxdesk_core_traits::metrics::LatencySamples;

/// Named stage with its samples (µs) and an optional budget (µs).
#[derive(Debug, Clone, Default)]
pub struct Stage {
    pub name: &'static str,
    pub budget_us: Option<u32>,
    pub samples: LatencySamples,
}

impl Stage {
    pub fn new(name: &'static str, budget_us: Option<u32>) -> Self {
        Self { name, budget_us, samples: LatencySamples::default() }
    }
    pub fn push(&mut self, us: u32) {
        self.samples.push(us);
    }
    pub fn p(&self, pct: f64) -> Option<u32> {
        self.samples.percentile(pct)
    }
}

fn ms(us: Option<u32>) -> String {
    us.map(|v| format!("{:.2}", v as f64 / 1000.0)).unwrap_or_else(|| "-".into())
}

/// Render a markdown table: stage | n | p50 | p95 | p99 | budget | ok(p95 ≤ budget).
pub fn latency_table(stages: &[Stage]) -> String {
    let mut s = String::from("| Stage | n | p50 ms | p95 ms | p99 ms | budget ms | p95 within budget |\n|---|---|---|---|---|---|---|\n");
    for st in stages {
        let p95 = st.p(95.0);
        let ok = match (st.budget_us, p95) {
            (Some(b), Some(v)) => {
                if v <= b {
                    "yes"
                } else {
                    "NO"
                }
            }
            _ => "-",
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            st.name,
            st.samples.len(),
            ms(st.p(50.0)),
            ms(p95),
            ms(st.p(99.0)),
            ms(st.budget_us),
            ok
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_marks_budget() {
        let mut a = Stage::new("encode", Some(3000));
        let mut b = Stage::new("decode", Some(1000));
        for i in 0..100 {
            a.push(1000 + i);
            b.push(2000 + i);
        }
        let t = latency_table(&[a, b]);
        assert!(t.contains("| encode | 100 | 1.05 |"));
        assert!(t.lines().nth(2).unwrap().ends_with("yes |"));
        assert!(t.lines().nth(3).unwrap().ends_with("NO |"));
    }
}
