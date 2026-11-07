# Agent Instructions: Video Digestion Workflow

## Scope and modality dispatch

Use this workflow for moving-image content, temporal events, or synchronized visual/audio digestion. It owns temporal coverage and synthesis; [vision](AGENTS.vision.md) and [audio](AGENTS.audio.md) own their methods and scoped ceilings.

Assigned inspectors use the [inspector profile](../profiles/AGENTS.inspector.md).

- Dispatch frame/sequence inspection under vision's ceiling and recording inspection under audio's. A combined inspection needs both capabilities and obeys the stricter cap, or is split.
- Media extraction, orchestration, and synthesis from text-only findings keep the general ceiling; transcripts, captions, or frame descriptions do not qualify for a modality exception or establish direct media coverage.

## Inputs and coverage plan

- Supply source paths/references and revisions, requested deliverable, duration, relevant tracks, known synchronization offsets, acceptance criteria, and temporal questions. Distinguish content summary, event detection, motion review, and playback verification.
- Verify decoding/playback support, timestamps, resolution, duration, and input limits. Preserve the source; record extracted frame/clip/audio ranges and transformations mapped to the original timeline, including variable frame timing.
- Use broad sampling for orientation, then continuous playback or sufficiently detailed sequences around consequential transitions. Sparse stills cannot establish absence of brief events or continuity of motion.
- Split by time range or evaluation dimension, overlapping intervals where events or speech span boundaries; retain inspected ranges, gaps, and resolution/time limitations per modality.

## Inspect and synthesize

1. Inspect visual sequences under the vision workflow and sound under the audio workflow, preserving modality-specific evidence and uncertainty (unreadable text, unheard passages, occlusion, events between samples).
2. Align findings on the source timeline using observed timestamps and known offsets. Verify any consequential claim linking speech, sound, and visible action; temporal proximity alone does not establish causation or speaker identity.
3. Reconcile cross-segment events and duplicated material. Produce the requested digest with timestamped events, key points, and decisions/actions, preserving conflicting evidence.
4. For video quality or playback acceptance, inspect motion, transitions, synchronization, seeking, and playback behavior. Use measurements for exact timing/frame-rate claims; extracted frames or an offline recording cannot establish live application behavior. Load [browser verification](AGENTS.browser.md) when an interactive application is involved.

## Findings and recheck

- Return a verdict per source/range/criterion with timestamps, artifact revision, inspected coverage by modality, evidence, and correction. Label sampled coverage; do not claim full-event coverage from a transcript or a few stills.
- Route repairs through the manager and recheck stable changed clips, affected boundaries/synchronization, and dependent digest conclusions under [shared validation](../guides/validation.md#shared-validation-rules).
- Use vision's repair allowance for visual defects and audio's for audio defects; other recovery uses [coordination](../guides/subagent-recovery.md#stalls-and-recovery). A retry addressing the same audiovisual failure counts against both allowances; switching modalities does not reset its count.
- Report inaccessible tracks or inadequate temporal coverage and complete unaffected checks. Leave dependent claims unverified when either required modality or time range is missing.
