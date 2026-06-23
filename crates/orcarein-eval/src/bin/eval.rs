//! `eval` — batch-run the toy suite across cache configs and write a CSV.
//!
//! Usage:
//!   cargo run -p orcarein-eval --bin eval -- \
//!     --configs economy,benchmark --repeat 3 --out results/ --model deepseek-v4-flash
//!
//! Reads the API key from $DEEPSEEK_API_KEY. With no key, only `--dry-run`
//! (Mock provider) works.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use orcarein_core::{CacheMode, DeepSeekProvider};
use orcarein_eval::metrics::aggregate;
use orcarein_eval::report::write_csv;
use orcarein_eval::runner::{run_case, standard_registry, RunConfig};
use orcarein_eval::task::toy_suite;

#[derive(Parser, Debug)]
#[command(about = "Batch eval runner for orcarein context/cache strategies")]
struct Args {
    /// Comma-separated cache configs: economy,benchmark
    #[arg(long, default_value = "economy,benchmark")]
    configs: String,
    /// Repeats per (task × config) cell.
    #[arg(long, default_value_t = 3)]
    repeat: usize,
    /// Output directory for the CSV + JSONL traces.
    #[arg(long, default_value = "results")]
    out: PathBuf,
    /// Model name (DeepSeek prices known: deepseek-v4-flash / -pro).
    #[arg(long, default_value = "deepseek-v4-flash")]
    model: String,
    /// Cap total cells (budget guard). 0 = no cap.
    #[arg(long, default_value_t = 0)]
    max_cases: usize,
    /// Max tool iterations per task.
    #[arg(long, default_value_t = 8)]
    max_iterations: usize,
}

fn parse_config(s: &str) -> Result<CacheMode> {
    match s {
        "economy" => Ok(CacheMode::Economy),
        "benchmark" => Ok(CacheMode::Benchmark),
        other => anyhow::bail!("unknown config '{other}' (want economy|benchmark)"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let configs: Vec<&str> = args.configs.split(',').filter(|s| !s.is_empty()).collect();

    let key = std::env::var("DEEPSEEK_API_KEY")
        .context("set $DEEPSEEK_API_KEY to run the eval against the real API")?;
    let provider = DeepSeekProvider::new(key);

    let registry = standard_registry();
    let defs = registry.definitions();
    let suite = toy_suite();

    std::fs::create_dir_all(&args.out)?;
    let run_id = format!("run-{}", std::process::id()); // stable within one invocation
    let trace_dir = args.out.join(&run_id);
    std::fs::create_dir_all(&trace_dir)?;

    let mut rows = Vec::new();
    let mut cells = 0usize;
    'outer: for case in &suite {
        // Allow the risky tools the toy suite needs.
        let allow = vec!["write_file".to_string(), "edit".to_string(), "read_file".to_string()];
        for cfg_name in &configs {
            let cache_mode = parse_config(cfg_name)?;
            for r in 0..args.repeat {
                if args.max_cases != 0 && cells >= args.max_cases {
                    break 'outer;
                }
                let cfg = RunConfig {
                    cache_mode,
                    model: args.model.clone(),
                    allow_tools: allow.clone(),
                    max_iterations: args.max_iterations,
                };
                let rec = run_case(case, &cfg, &provider, &registry, &defs).await?;
                // Persist the full trace as JSONL.
                let trace_path =
                    trace_dir.join(format!("{}-{}-{}.jsonl", case.id, cfg_name, r));
                let lines: Vec<String> = rec
                    .trace
                    .iter()
                    .map(|e| serde_json::to_string(&format!("{e:?}")).unwrap())
                    .collect();
                std::fs::write(&trace_path, lines.join("\n"))?;

                let m = aggregate(
                    &rec.usage, rec.verdict, rec.steps, rec.wall_ms, rec.ttft_ms,
                    &case.id, cfg_name, r, &args.model,
                );
                println!(
                    "{:>14} {:>9} #{r}: {} hit={:.2} ${:.5}",
                    case.id, cfg_name, m.verdict, m.cache_hit_rate, m.spent_usd
                );
                rows.push(m);
                cells += 1;
            }
        }
    }

    let csv_path = args.out.join("metrics.csv");
    write_csv(&rows, &csv_path)?;
    println!("\nwrote {} rows -> {}", rows.len(), csv_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_maps_known_names() {
        assert!(matches!(parse_config("economy").unwrap(), CacheMode::Economy));
        assert!(matches!(parse_config("benchmark").unwrap(), CacheMode::Benchmark));
        assert!(parse_config("bogus").is_err());
    }
}
