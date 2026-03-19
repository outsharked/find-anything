use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use find_common::config::{default_server_config_path, parse_server_config};
use find_content_store::bench::{
    bench_read, bench_write, ReadBenchOpts, ReadBenchResult, WriteBenchOpts, WriteBenchResult,
};
use find_content_store::{open_backend, ContentStore};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "find-test", about = "Server-side testing and benchmarking for find-anything", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Benchmark content store read/write performance
    BenchStorage {
        /// Path to server.toml
        #[arg(long, default_value_t = default_server_config_path())]
        config: String,

        /// Override data_dir from config
        #[arg(long)]
        data_dir: Option<PathBuf>,

        /// URL of a newline-delimited word list for synthetic text generation.
        /// Words should be frequency-ordered (most common first).
        #[arg(long, default_value = "https://raw.githubusercontent.com/first20hours/google-10000-english/master/google-10000-english-no-swears.txt")]
        wordlist_url: String,

        /// Named backend(s) to benchmark (default: all configured)
        #[arg(long)]
        backend: Vec<String>,

        /// What to measure: write, read, or all
        #[arg(long, default_value = "all")]
        mode: BenchMode,

        /// Number of blobs to write
        #[arg(long, default_value = "1000")]
        blobs: usize,

        /// Median blob size in KB (log-normal distribution)
        #[arg(long, default_value = "4")]
        blob_size_kb: usize,

        /// Log-normal σ for blob size spread (0 = fixed size, 1.5 = realistic spread)
        #[arg(long, default_value = "1.5")]
        blob_size_sigma: f64,

        /// Number of get_lines() calls in the read phase
        #[arg(long, default_value = "2000")]
        reads: usize,

        /// Reader thread count (repeatable for a concurrency sweep, e.g. --concurrency 1 --concurrency 8)
        #[arg(long, default_value = "1")]
        concurrency: Vec<usize>,

        /// RNG seed (same seed → identical synthetic data every run)
        #[arg(long, default_value = "0")]
        seed: u64,

        /// Emit JSON instead of a human-readable table
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum BenchMode {
    Write,
    Read,
    All,
}

// ── Result row ────────────────────────────────────────────────────────────────

struct BenchRow {
    name: String,
    write: Option<WriteBenchResult>,
    reads: Vec<ReadBenchResult>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::BenchStorage {
            config,
            data_dir,
            wordlist_url,
            backend,
            mode,
            blobs,
            blob_size_kb,
            blob_size_sigma,
            reads,
            mut concurrency,
            seed,
            json,
        } => {
            let wordlist = fetch_wordlist(&wordlist_url)?;
            run_bench_storage(
                &config, data_dir, backend, mode, blobs, blob_size_kb, blob_size_sigma,
                reads, &mut concurrency, seed, json, wordlist,
            )
        }
    }
}

fn fetch_wordlist(url: &str) -> Result<Vec<String>> {
    eprintln!("Fetching wordlist from {url}…");
    let body = reqwest::blocking::get(url)
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("bad status from {url}"))?
        .text()
        .context("reading wordlist body")?;
    let words: Vec<String> = body.lines()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    anyhow::ensure!(!words.is_empty(), "wordlist at {url} is empty");
    eprintln!("  {} words loaded.", words.len());
    Ok(words)
}

#[allow(clippy::too_many_arguments)]
fn run_bench_storage(
    config_path: &str,
    data_dir_override: Option<PathBuf>,
    backend_filter: Vec<String>,
    mode: BenchMode,
    blobs: usize,
    blob_size_kb: usize,
    blob_size_sigma: f64,
    reads: usize,
    concurrency: &mut Vec<usize>,
    seed: u64,
    json: bool,
    wordlist: Vec<String>,
) -> Result<()> {
    let config_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading config: {config_path}"))?;
    let (server_cfg, _warnings) = parse_server_config(&config_str)?;

    let data_dir = data_dir_override
        .unwrap_or_else(|| PathBuf::from(&server_cfg.server.data_dir));

    // Select backends.
    let all_backends = &server_cfg.storage.backends;
    let selected: Vec<_> = if backend_filter.is_empty() {
        all_backends.iter().collect()
    } else {
        all_backends
            .iter()
            .filter(|b| backend_filter.contains(&b.name))
            .collect()
    };
    anyhow::ensure!(!selected.is_empty(), "no matching backends found");

    // Normalise concurrency list.
    if concurrency.is_empty() {
        concurrency.push(1);
    }
    concurrency.sort_unstable();
    concurrency.dedup();

    // Open stores.
    struct NamedStore {
        name: String,
        store: Arc<dyn ContentStore>,
    }

    let mut named_stores: Vec<NamedStore> = Vec::new();
    for b in &selected {
        let dir = if all_backends.len() == 1 {
            data_dir.clone()
        } else {
            data_dir.join("stores").join(&b.name)
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating store dir for '{}'", b.name))?;
        named_stores.push(NamedStore { name: b.name.clone(), store: open_backend(b, &dir)? });
    }

    // Run benchmarks.
    let mut rows: Vec<BenchRow> = Vec::new();

    for ns in &named_stores {
        eprintln!("→ {}", ns.name);
        let mut row = BenchRow { name: ns.name.clone(), write: None, reads: vec![] };

        let write_opts = WriteBenchOpts {
            num_blobs: blobs,
            blob_size_bytes: blob_size_kb * 1024,
            blob_size_sigma,
            seed,
            wordlist: wordlist.clone(),
        };

        // Write phase — always run to obtain keys for the read phase.
        let keys = match mode {
            BenchMode::Read => {
                eprintln!("  populating store for read benchmark…");
                let (_, keys) = bench_write(ns.store.as_ref(), &write_opts)?;
                keys
            }
            BenchMode::Write | BenchMode::All => {
                eprint!("  write ({blobs} blobs, median {blob_size_kb} KB, σ={blob_size_sigma:.1})… ");
                let (result, keys) = bench_write(ns.store.as_ref(), &write_opts)?;
                eprintln!("{:.1} MB/s  ({:.0} blobs/s)", result.mb_per_sec(), result.blobs_per_sec());
                row.write = Some(result);
                keys
            }
        };

        // Read phase.
        if matches!(mode, BenchMode::Read | BenchMode::All) {
            for &c in concurrency.iter() {
                eprint!("  read (c={c}, {reads} calls)… ");
                let result = bench_read(ns.store.as_ref(), &ReadBenchOpts {
                    num_reads: reads,
                    concurrency: c,
                    keys: keys.clone(),
                    seed,
                })?;
                eprintln!(
                    "p50={:.2}ms  p95={:.2}ms  p99={:.2}ms",
                    ms(result.percentile(0.50)),
                    ms(result.percentile(0.95)),
                    ms(result.percentile(0.99)),
                );
                row.reads.push(result);
            }
        }

        rows.push(row);
    }

    // Print results.
    if json {
        print_json(&rows, concurrency);
    } else {
        print_table(&rows, concurrency);
    }

    Ok(())
}

// ── Formatting ────────────────────────────────────────────────────────────────

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn fmt_ms(d: Duration) -> String {
    format!("{:.2}ms", ms(d))
}

fn print_table(rows: &[BenchRow], concurrency: &[usize]) {
    let w = 16usize; // backend name column width
    let mut header = format!("{:<w$}  {:>8}  {:>9}", "Backend", "MB/s", "blobs/s");
    for &c in concurrency {
        header.push_str(&format!("  {:^25}", format!("Read (c={c})")));
    }
    println!("{header}");

    let mut sub = format!("{:<w$}  {:>8}  {:>9}", "", "", "");
    for _ in concurrency {
        sub.push_str(&format!("  {:>7}  {:>7}  {:>7}", "p50", "p95", "p99"));
    }
    println!("{sub}");
    println!("{}", "─".repeat(header.len().min(120)));

    for row in rows {
        let (mb, bps) = row.write.as_ref().map_or(
            ("-".to_string(), "-".to_string()),
            |w| (format!("{:.1}", w.mb_per_sec()), format!("{:.0}", w.blobs_per_sec())),
        );
        let mut line = format!("{:<w$}  {:>8}  {:>9}", row.name, mb, bps);
        for r in &row.reads {
            line.push_str(&format!(
                "  {:>7}  {:>7}  {:>7}",
                fmt_ms(r.percentile(0.50)),
                fmt_ms(r.percentile(0.95)),
                fmt_ms(r.percentile(0.99)),
            ));
        }
        println!("{line}");
    }
}

fn print_json(rows: &[BenchRow], concurrency: &[usize]) {
    let mut out = String::from("[\n");
    for (ri, row) in rows.iter().enumerate() {
        out.push_str("  {\n");
        out.push_str(&format!("    \"backend\": {:?},\n", row.name));
        if let Some(w) = &row.write {
            out.push_str(&format!(
                "    \"write\": {{ \"mb_per_sec\": {:.3}, \"blobs_per_sec\": {:.1} }},\n",
                w.mb_per_sec(), w.blobs_per_sec()
            ));
        }
        out.push_str("    \"read\": [\n");
        for (i, r) in row.reads.iter().enumerate() {
            let c = concurrency.get(i).copied().unwrap_or(0);
            out.push_str(&format!(
                "      {{ \"concurrency\": {c}, \"p50_ms\": {:.4}, \"p95_ms\": {:.4}, \"p99_ms\": {:.4} }}",
                ms(r.percentile(0.50)),
                ms(r.percentile(0.95)),
                ms(r.percentile(0.99)),
            ));
            if i + 1 < row.reads.len() { out.push(','); }
            out.push('\n');
        }
        out.push_str("    ]\n  }");
        if ri + 1 < rows.len() { out.push(','); }
        out.push('\n');
    }
    out.push(']');
    println!("{out}");
}
