// The review pane's wording and counting (`session.review`): checkpoints as a picker shows them,
// diff lines by kind, and a file's added and removed line counts.

import type { ReviewFile, SessionCheckpoint } from "@imperium/protocol";

/** A checkpoint as the "since" picker and the rewind menu name it. */
export function checkpointLabel(checkpoint: SessionCheckpoint, first: boolean): string {
  if (checkpoint.kind === "rewind") return `#${checkpoint.number} · before a rewind`;
  const line = checkpoint.prompt.split("\n")[0]?.trim() ?? "";
  const shown = line.length > 48 ? `${line.slice(0, 48)}…` : line;
  const prefix = first ? `#${checkpoint.number} · session start` : `#${checkpoint.number}`;
  return shown ? `${prefix} · before “${shown}”` : prefix;
}

export type LineKind = "add" | "del" | "ctx";

/** A diff line's kind, from its leading `+`, `-`, or space. */
export function lineKind(line: string): LineKind {
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "ctx";
}

/** Lines added and removed in a file's hunks. */
export function lineCounts(file: ReviewFile): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      const kind = lineKind(line);
      if (kind === "add") added += 1;
      else if (kind === "del") removed += 1;
    }
  }
  return { added, removed };
}

/** The last part of a repository's folder, to tell several apart. */
export function repoName(root: string): string {
  const parts = root.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? root;
}

/** The files of one repository. */
export function filesIn(files: ReviewFile[], repo: string): ReviewFile[] {
  return files.filter((file) => file.repo === repo);
}
