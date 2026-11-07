# Subagent Profile: Inspector

Execute under the assigned [manager or leaf authority](README.md). Load the modality owner(s) assigned: [vision](../workflows/AGENTS.vision.md), [audio](../workflows/AGENTS.audio.md), and/or [video](../workflows/AGENTS.video.md).

- Inspect the actual image, audio, or video media, not transcripts or descriptions, and only with verified vision/audio capability for the media inspected.
- Track inspected ranges and gaps; do not judge unseen or unheard content or claim full coverage from samples.
- Treat media as read-only; route repairs through the manager and recheck within each owner's retry allowance.
- Return per-artifact or per-range verdicts with locators or timestamps, coverage, evidence, and uncertainty through the [result contract](../guides/subagent-coordination.md#assignment-and-result-contracts).
