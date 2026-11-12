//! `imperium resources`: what the machine and the sessions are using, once or live.

use std::fmt::Write as _;
use std::io::IsTerminal;

use anyhow::Context;
use imperium_protocol::methods::{
    ModelUnload, ModelUnloadParams, RESOURCES_CHANGED, ResourceSnapshot, SystemResources,
    SystemResourcesParams, SystemResourcesSubscribe, SystemResourcesSubscribeParams,
};

use crate::{block_on, connect};

pub fn show(watch: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        if !watch {
            let snapshot = client
                .call::<SystemResources>(SystemResourcesParams {})
                .await?;
            print!("{}", render(&snapshot));
            return Ok(());
        }
        client
            .call::<SystemResourcesSubscribe>(SystemResourcesSubscribeParams { on: true })
            .await?;
        let live = std::io::stdout().is_terminal();
        loop {
            let notification = client.next_notification().await?;
            if notification.method != RESOURCES_CHANGED {
                continue;
            }
            let snapshot: ResourceSnapshot = serde_json::from_value(notification.params)
                .context("could not read the resource snapshot")?;
            if live {
                // Clear the screen and draw from the top.
                print!("\x1b[2J\x1b[H");
                println!("Ctrl+C stops watching.\n");
            }
            print!("{}", render(&snapshot));
            if !live {
                println!();
            }
        }
    })
}

pub fn unload(model: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        client
            .call::<ModelUnload>(ModelUnloadParams {
                model: model.clone(),
            })
            .await?;
        println!("unloaded {model}");
        Ok(())
    })
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

fn percent(used: u64, total: u64) -> String {
    if total == 0 {
        return "-".into();
    }
    format!("{:.0}%", used as f64 * 100.0 / total as f64)
}

fn seconds(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// The snapshot as plain text, one section per kind of resource.
pub fn render(snapshot: &ResourceSnapshot) -> String {
    let mut out = String::new();
    let cpu = &snapshot.cpu;
    let cores: Vec<String> = cpu
        .per_core
        .iter()
        .map(|core| format!("{core:.0}"))
        .collect();
    let _ = writeln!(
        out,
        "CPU   {:>5.1}%  ({} cores: {})",
        cpu.total_percent,
        cpu.per_core.len(),
        cores.join(" ")
    );
    let memory = &snapshot.memory;
    let _ = writeln!(
        out,
        "RAM   {} / {} ({})   swap {} / {}",
        gib(memory.ram_used_bytes),
        gib(memory.ram_total_bytes),
        percent(memory.ram_used_bytes, memory.ram_total_bytes),
        gib(memory.swap_used_bytes),
        gib(memory.swap_total_bytes),
    );
    if snapshot.gpus.is_empty() {
        let _ = writeln!(out, "GPU   none reported");
    }
    for gpu in &snapshot.gpus {
        let utilization = gpu
            .utilization_percent
            .map_or_else(|| "-".into(), |u| format!("{u:.0}%"));
        let temperature = gpu
            .temperature_c
            .map(|t| format!("  {t:.0}°C"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "GPU{}  {}  util {}  VRAM {} / {} ({}){}",
            gpu.index,
            gpu.name,
            utilization,
            gib(gpu.vram_used_bytes),
            gib(gpu.vram_total_bytes),
            percent(gpu.vram_used_bytes, gpu.vram_total_bytes),
            temperature,
        );
    }

    let _ = writeln!(out, "\nProcesses");
    for process in &snapshot.processes {
        let what = process.model.as_deref().map_or_else(
            || process.role.clone(),
            |model| format!("{} {model}", process.role),
        );
        let vram = process
            .vram_bytes
            .map(|bytes| format!("  VRAM {}", gib(bytes)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:>7}  {:<24} CPU {:>5.1}%  RAM {}{}",
            process.pid,
            what,
            process.cpu_percent,
            gib(process.ram_bytes),
            vram
        );
    }

    let _ = writeln!(out, "\nLocal models");
    if snapshot.local_models.is_empty() {
        let _ = writeln!(out, "  none loaded");
    }
    for model in &snapshot.local_models {
        let mut state = match (model.slots_in_use, model.unload_in_ms) {
            (0, Some(left)) => format!("idle, unloads in {}", seconds(left)),
            (0, None) if model.claimed => "held by a turn in progress".into(),
            (0, None) => "idle".into(),
            (busy, _) => format!("{busy}/{} slots busy", model.slots),
        };
        if model.queued > 0 {
            state.push_str(&format!(", {} queued", model.queued));
        }
        match (&model.draft_model, &model.draft_notice) {
            (Some(draft), _) => state.push_str(&format!(", draft {draft}")),
            (None, Some(notice)) => state.push_str(&format!(", {notice}")),
            (None, None) => {}
        }
        let sessions = if model.sessions.is_empty() {
            String::new()
        } else {
            format!("  sessions: {}", model.sessions.join(", "))
        };
        let _ = writeln!(
            out,
            "  {}:{}  {}{} ({} GPU layers)  ctx {}  KV pool {}/{}  VRAM {}  RAM {}  {}{}",
            model.runtime,
            model.model,
            model.placement,
            if model.cpu_pinned { ", CPU lane" } else { "" },
            model.gpu_layers,
            model.context,
            model.pool_reserved_tokens,
            model.pool_tokens,
            gib(model.vram_bytes),
            gib(model.ram_bytes),
            state,
            sessions
        );
    }

    let _ = writeln!(out, "\nEndpoints (last 15 minutes)");
    if snapshot.endpoints.is_empty() {
        let _ = writeln!(out, "  none in use");
    }
    for endpoint in &snapshot.endpoints {
        let host = endpoint
            .host
            .as_deref()
            .map(|host| format!(" ({host})"))
            .unwrap_or_default();
        let latency = endpoint
            .last_latency_ms
            .map_or_else(|| "-".into(), |ms| format!("{ms} ms"));
        let _ = writeln!(
            out,
            "  {}{}  {} requests  {} in / {} out tokens  last {}  {} errors  sessions: {}",
            endpoint.spec,
            host,
            endpoint.requests,
            endpoint.prompt_tokens,
            endpoint.completion_tokens,
            latency,
            endpoint.errors,
            if endpoint.sessions.is_empty() {
                "-".into()
            } else {
                endpoint.sessions.join(", ")
            },
        );
        if let Some(error) = &endpoint.last_error {
            let _ = writeln!(out, "    last error: {error}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::methods::{
        ActiveEndpoint, ActiveLocalModel, CpuUsage, GpuUsage, MemoryUsage, ProcessUsage,
    };

    #[test]
    fn a_snapshot_renders_every_section() {
        let gib = 1024 * 1024 * 1024;
        let snapshot = ResourceSnapshot {
            taken_at_ms: 0,
            cpu: CpuUsage {
                total_percent: 12.5,
                per_core: vec![20.0, 5.0],
            },
            memory: MemoryUsage {
                ram_used_bytes: 8 * gib,
                ram_total_bytes: 32 * gib,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
            },
            gpus: vec![GpuUsage {
                index: 0,
                name: "RTX".into(),
                vendor: "nvidia".into(),
                utilization_percent: Some(40.0),
                vram_used_bytes: 4 * gib,
                vram_total_bytes: 16 * gib,
                temperature_c: Some(55.0),
            }],
            processes: vec![ProcessUsage {
                pid: 7,
                name: "llama-server".into(),
                role: "llamacpp".into(),
                model: Some("qwen".into()),
                cpu_percent: 3.0,
                ram_bytes: gib,
                vram_bytes: Some(3 * gib),
            }],
            local_models: vec![ActiveLocalModel {
                model: "qwen".into(),
                runtime: "llamacpp".into(),
                placement: "gpu".into(),
                gpu_layers: 99,
                context: 8192,
                pool_tokens: 16384,
                pool_reserved_tokens: 0,
                vram_bytes: 3 * gib,
                ram_bytes: 0,
                slots: 2,
                slots_in_use: 0,
                queued: 0,
                cpu_pinned: false,
                draft_model: None,
                draft_notice: None,
                claimed: false,
                sessions: vec!["s1".into()],
                pid: Some(7),
                idle_ms: Some(1000),
                unload_in_ms: Some(125_000),
                can_unload: true,
            }],
            endpoints: vec![ActiveEndpoint {
                endpoint: "openai".into(),
                host: Some("api.openai.com".into()),
                model: "gpt".into(),
                spec: "openai:gpt".into(),
                sessions: vec!["s2".into()],
                requests: 3,
                prompt_tokens: 900,
                completion_tokens: 30,
                last_latency_ms: Some(812),
                errors: 1,
                last_error: Some("rate limited".into()),
            }],
        };
        let mut drafted = snapshot.clone();
        drafted.local_models[0].draft_model = Some("qwen-0.5b".into());
        assert!(render(&drafted).contains("idle, unloads in 2m05s, draft qwen-0.5b  sessions"));
        drafted.local_models[0].draft_model = None;
        drafted.local_models[0].draft_notice = Some("Draft model x is not used.".into());
        assert!(render(&drafted).contains(", Draft model x is not used.  sessions"));
        let text = render(&snapshot);
        assert!(text.contains("RAM   8.0 GiB / 32.0 GiB (25%)"), "{text}");
        assert!(text.contains("GPU0  RTX  util 40%"), "{text}");
        assert!(text.contains("llamacpp qwen"), "{text}");
        assert!(
            text.contains("idle, unloads in 2m05s  sessions: s1"),
            "{text}"
        );
        assert!(
            text.contains("openai:gpt (api.openai.com)  3 requests"),
            "{text}"
        );
        assert!(text.contains("last error: rate limited"), "{text}");
    }
}
