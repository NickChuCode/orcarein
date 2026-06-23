//! CSV export for [`TaskMetrics`] rows.

use std::path::Path;

use anyhow::Result;

use crate::metrics::TaskMetrics;

/// Writes one header row + one row per metric to `out`. Column order matches
/// the `TaskMetrics` field order.
pub fn write_csv(rows: &[TaskMetrics], out: &Path) -> Result<()> {
    let mut w = csv::Writer::from_path(out)?;
    w.write_record([
        "task_id",
        "config",
        "repeat",
        "verdict",
        "steps",
        "prompt_tokens",
        "cached_tokens",
        "cache_hit_rate",
        "completion_tokens",
        "spent_usd",
        "saved_usd",
        "input_saved_pct",
        "wall_ms",
        "ttft_ms",
    ])?;
    for r in rows {
        w.write_record([
            r.task_id.clone(),
            r.config.clone(),
            r.repeat.to_string(),
            r.verdict.to_string(),
            r.steps.to_string(),
            r.prompt_tokens.to_string(),
            r.cached_tokens.to_string(),
            format!("{:.6}", r.cache_hit_rate),
            r.completion_tokens.to_string(),
            format!("{:.6}", r.spent_usd),
            format!("{:.6}", r.saved_usd),
            format!("{:.6}", r.input_saved_pct),
            r.wall_ms.to_string(),
            r.ttft_ms.map(|v| v.to_string()).unwrap_or_default(),
        ])?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::TaskMetrics;
    use crate::task::Verdict;

    fn row(id: &str, cfg: &str, hit_rate: f64) -> TaskMetrics {
        TaskMetrics {
            task_id: id.into(),
            config: cfg.into(),
            repeat: 0,
            verdict: Verdict::Pass,
            steps: 2,
            prompt_tokens: 1000,
            cached_tokens: 800,
            cache_hit_rate: hit_rate,
            completion_tokens: 50,
            spent_usd: 0.001,
            saved_usd: 0.01,
            input_saved_pct: 0.9,
            wall_ms: 500,
            ttft_ms: Some(40),
        }
    }

    #[test]
    fn writes_header_and_one_row_per_metric() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.csv");
        let rows = vec![row("a", "economy", 0.8), row("a", "benchmark", 0.0)];
        write_csv(&rows, &out).unwrap();

        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows");
        assert!(lines[0].starts_with("task_id,config,repeat,verdict"));
        assert!(lines[1].contains("economy"));
        assert!(lines[2].contains("benchmark"));
    }
}
