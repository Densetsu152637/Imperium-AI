//! `imperium models files`, `imperium models pull`, and `imperium models cancel`: Hugging Face
//! downloads, which the daemon runs so they survive the terminal closing.

use std::io::{IsTerminal, Write};

use anyhow::Context;
use imperium_protocol::methods::{
    DownloadJob, HfFiles, HfFilesParams, MODEL_DOWNLOAD_PROGRESS, ModelDownload,
    ModelDownloadCancel, ModelDownloadCancelParams, ModelDownloadParams,
};

use crate::{block_on, connect};

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB)
}

fn fit(fit: &str) -> &'static str {
    match fit {
        "gpu" => "fits on GPU",
        "partial" => "fits split between GPU and RAM",
        "cpu" => "fits in RAM (CPU)",
        _ => "does not fit",
    }
}

pub fn files(repo: String, revision: Option<String>) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<HfFiles>(HfFilesParams { repo, revision })
            .await?;
        if listing.variants.is_empty() {
            println!(
                "{} has no GGUF or safetensors weights. Its files:",
                listing.repo
            );
            for file in &listing.files {
                println!("  {:<60} {:>10}", file.path, gib(file.size_bytes));
            }
            return Ok(());
        }
        println!("{} at {}:", listing.repo, listing.revision);
        for variant in &listing.variants {
            let default = if listing.default.as_ref() == Some(&variant.name) {
                " (default)"
            } else {
                ""
            };
            let parts = match variant.files.len() {
                1 => String::new(),
                n => format!(", {n} files"),
            };
            println!(
                "  {:<16} {:>10}  {}:{}  {}{parts}{default}",
                variant.name,
                gib(variant.size_bytes),
                variant.runtime,
                variant.model_id,
                fit(&variant.fit),
            );
        }
        for projector in &listing.projectors {
            println!(
                "  projector {} ({}); add --mmproj to download it",
                projector.path,
                gib(projector.size_bytes)
            );
        }
        println!("\nDownloads go to {}", listing.download_dir);
        println!(
            "Pull one with: imperium models pull {} --quant <name>",
            listing.repo
        );
        Ok(())
    })
}

pub fn pull(params: ModelDownloadParams) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let job = client.call::<ModelDownload>(params).await?;
        println!(
            "downloading {} {} ({}, {} file{}) to {}",
            job.repo,
            job.variant,
            gib(job.total_bytes),
            job.files.len(),
            if job.files.len() == 1 { "" } else { "s" },
            job.dir
        );
        println!(
            "job {}: Ctrl+C stops watching; the daemon keeps downloading (stop it with `imperium models cancel {}`)",
            job.job_id, job.job_id
        );
        let live = std::io::stdout().is_terminal();
        let mut last_line = String::new();
        let ended = loop {
            let notification = client.next_notification().await?;
            if notification.method != MODEL_DOWNLOAD_PROGRESS {
                continue;
            }
            let update: DownloadJob = serde_json::from_value(notification.params)
                .context("could not read download progress")?;
            if update.job_id != job.job_id {
                continue;
            }
            if update.state != "running" {
                break update;
            }
            let line = progress_line(&update);
            if live {
                print!("\r{line:<100}");
                std::io::stdout().flush().ok();
            } else if line != last_line {
                println!("{line}");
            }
            last_line = line;
        };
        if live {
            println!();
        }
        match ended.state.as_str() {
            "done" => {
                println!("done: use it as {}", ended.model);
                Ok(())
            }
            "cancelled" => anyhow::bail!("the download was cancelled; pulling again resumes it"),
            _ => anyhow::bail!(
                "the download failed: {}",
                ended.message.as_deref().unwrap_or("no reason given")
            ),
        }
    })
}

/// `[42%] 3.10 / 7.35 GiB  model-Q4_K_M.gguf`; whole percentages so a log gets few lines.
fn progress_line(job: &DownloadJob) -> String {
    let percent = (job.done_bytes * 100)
        .checked_div(job.total_bytes)
        .unwrap_or(0);
    format!(
        "[{percent:>3}%] {} / {}  {}",
        gib(job.done_bytes),
        gib(job.total_bytes),
        job.file.as_deref().unwrap_or("")
    )
}

pub fn cancel(job_id: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<ModelDownloadCancel>(ModelDownloadCancelParams {
                job_id: job_id.clone(),
            })
            .await?;
        if result.found {
            println!("cancelled {job_id}; its partial files stay, so pulling again resumes");
        } else {
            println!("no running download {job_id}");
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_show_whole_percentages() {
        let job = DownloadJob {
            job_id: "dl-1".into(),
            repo: "o/r".into(),
            revision: "main".into(),
            variant: "Q4_K_M".into(),
            model: "llamacpp:r-Q4_K_M".into(),
            files: vec!["r-Q4_K_M.gguf".into()],
            dir: "/m/o/r".into(),
            file: Some("r-Q4_K_M.gguf".into()),
            file_done_bytes: 0,
            file_total_bytes: 0,
            done_bytes: 1 << 30,
            total_bytes: 4 << 30,
            state: "running".into(),
            message: None,
        };
        assert_eq!(
            progress_line(&job),
            "[ 25%] 1.00 GiB / 4.00 GiB  r-Q4_K_M.gguf"
        );
        let empty = DownloadJob {
            total_bytes: 0,
            ..job
        };
        assert!(progress_line(&empty).starts_with("[  0%]"));
    }
}
