//! `imperium models bench [<id>]`: benchmarks installed local models through the daemon, which
//! loads each one the way any call does (as a background call, waiting for a quiet machine) and
//! saves the numbers; `--results` lists the saved ones without running anything. Ctrl+C
//! cancels a running benchmark.

use anyhow::Context;
use imperium_protocol::methods::{
    BenchResult, BenchRun, MODEL_BENCH_PROGRESS, ModelBench, ModelBenchCancel,
    ModelBenchCancelParams, ModelBenchParams, ModelBenchResults, ModelBenchResultsParams,
};

use crate::{block_on, connect};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Runs the benchmark and prints each model's result as it arrives.
pub fn run(model: Option<String>, warm: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let started = client
            .call::<ModelBench>(ModelBenchParams {
                model,
                cold: Some(!warm),
            })
            .await?;
        println!(
            "{}: benchmarking {} ({}). Ctrl+C cancels it.",
            started.run_id,
            plural(started.models.len(), "model"),
            started.models.join(", ")
        );
        println!("This loads each model, so it takes a while and may unload idle models.");
        let mut printed = 0;
        let mut last_step = String::new();
        let ended = loop {
            let notification = tokio::select! {
                notification = client.next_notification() => notification?,
                _ = tokio::signal::ctrl_c() => {
                    let cancelled = client
                        .call::<ModelBenchCancel>(ModelBenchCancelParams {
                            run_id: started.run_id.clone(),
                        })
                        .await?;
                    println!(
                        "{}",
                        if cancelled.found { "cancelled" } else { "it had already ended" }
                    );
                    return Ok(());
                }
            };
            if notification.method != MODEL_BENCH_PROGRESS {
                continue;
            }
            let update: BenchRun = serde_json::from_value(notification.params)
                .context("could not read benchmark progress")?;
            if update.run_id != started.run_id {
                continue;
            }
            for result in &update.results[printed..] {
                print_result(result);
            }
            printed = update.results.len();
            if update.state != "running" {
                break update;
            }
            let step = match (&update.current, &update.step) {
                (Some(model), Some(step)) => format!("{model}: {step}"),
                _ => String::new(),
            };
            if !step.is_empty() && step != last_step {
                println!("  ... {step}");
                last_step = step;
            }
        };
        match ended.state.as_str() {
            "done" => Ok(()),
            "cancelled" => anyhow::bail!("the benchmark was cancelled"),
            _ => anyhow::bail!(
                "the benchmark failed: {}",
                ended.message.as_deref().unwrap_or("no reason given")
            ),
        }
    })
}

/// Lists the saved results, newest first, optionally for one model.
pub fn results(model: Option<String>) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<ModelBenchResults>(ModelBenchResultsParams { model })
            .await?;
        if listing.results.is_empty() {
            println!("No saved benchmark results. Run one with `imperium models bench`.");
            return Ok(());
        }
        println!("This machine is {}.", listing.hardware_fingerprint);
        for result in &listing.results {
            print_result(result);
        }
        Ok(())
    })
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn opt(value: Option<f64>, format: impl Fn(f64) -> String) -> String {
    value.map_or_else(|| "-".into(), format)
}

/// One model's result as lines of text.
pub(crate) fn describe(result: &BenchResult) -> Vec<String> {
    let mut lines = vec![format!(
        "{}  {}  [{}]  {}",
        result.model_id, result.date, result.hardware_fingerprint, result.hardware
    )];
    if let Some(error) = &result.error {
        lines.push(format!("  failed: {error}"));
        return lines;
    }
    lines.push(format!(
        "  load {}   first token {}   prompt {}   generation {}",
        opt(result.load_s, |s| format!("{s:.1} s")),
        opt(result.ttft_ms, |ms| format!("{ms:.0} ms")),
        opt(result.prompt_tok_s, |rate| format!("{rate:.0} tok/s")),
        opt(result.gen_tok_s, |rate| format!("{rate:.1} tok/s")),
    ));
    let memory =
        |bytes: Option<u64>| opt(bytes.map(|b| b as f64), |b| format!("{:.2} GiB", b / GIB));
    lines.push(format!(
        "  placement {}   peak RAM {}   peak VRAM {}   machine {}",
        result.placement,
        memory(result.peak_ram_bytes),
        memory(result.peak_vram_bytes),
        result.machine
    ));
    for prompt in &result.prompts {
        lines.push(format!(
            "    {:<7} {:>5} prompt tok  {:>4} out tok  first token {:>6.0} ms  prompt {}  gen {}",
            prompt.name,
            prompt.prompt_tokens,
            prompt.completion_tokens,
            prompt.ttft_ms,
            opt(prompt.prompt_tok_s, |r| format!("{r:.0} tok/s")),
            opt(prompt.gen_tok_s, |r| format!("{r:.1} tok/s")),
        ));
    }
    lines
}

fn print_result(result: &BenchResult) {
    for line in describe(result) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::methods::BenchPrompt;

    fn result() -> BenchResult {
        BenchResult {
            model_id: "tiny".into(),
            run_id: "bench-1".into(),
            date: "2026-11-12T01:02:03Z".into(),
            hardware_fingerprint: "abc123".into(),
            hardware: "RTX 24 GB, 64 GB RAM".into(),
            runtime: "llamacpp".into(),
            placement: "gpu".into(),
            machine: "quiet".into(),
            load_s: Some(4.25),
            ttft_ms: Some(212.0),
            prompt_tok_s: Some(480.0),
            gen_tok_s: Some(19.5),
            peak_ram_bytes: Some(2 * 1024 * 1024 * 1024),
            peak_vram_bytes: None,
            prompts: vec![BenchPrompt {
                name: "short".into(),
                prompt_tokens: 20,
                completion_tokens: 5,
                ttft_ms: 212.0,
                prompt_tok_s: Some(94.0),
                gen_tok_s: None,
            }],
            error: None,
        }
    }

    #[test]
    fn a_result_shows_each_metric_and_marks_what_was_not_measured() {
        let lines = describe(&result());
        assert!(
            lines[0].starts_with("tiny  2026-11-12T01:02:03Z  [abc123]"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("load 4.2 s") || lines[1].contains("load 4.3 s"),
            "{lines:?}"
        );
        assert!(lines[1].contains("first token 212 ms"));
        assert!(lines[1].contains("prompt 480 tok/s"));
        assert!(lines[1].contains("generation 19.5 tok/s"));
        assert!(lines[2].contains("peak RAM 2.00 GiB") && lines[2].contains("peak VRAM -"));
        assert!(lines[3].contains("gen -"), "{lines:?}");
    }

    #[test]
    fn a_failed_model_shows_why() {
        let failed = BenchResult {
            error: Some("could not load tiny".into()),
            ..result()
        };
        let lines = describe(&failed);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("failed: could not load tiny"));
    }
}
