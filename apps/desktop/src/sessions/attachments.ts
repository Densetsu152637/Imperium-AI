// Files attached in the composer before a message is sent: each becomes a chip that uploads
// (`session.attach`), then sends with the message (`session.send`'s `attachments`) or is removed.
// The state changes are pure functions so they can be tested without a DOM; the browser helpers
// at the end read files and draw thumbnails.

import type { Attachment, SessionAttachResult } from "@imperium/protocol";

/** One file in the composer. */
export type Chip = {
  /** Local key, stable while the chip exists. */
  key: string;
  name: string;
  state: "uploading" | "ready" | "failed";
  /** The staged file, once the daemon accepted it. */
  attachment?: Attachment;
  /** Whether the session's model sees images (from `session.attach`). */
  vision?: boolean;
  /** A small `data:` thumbnail for images. */
  preview?: string;
  error?: string;
};

/** Adds a chip for a file that is being uploaded. */
export function addUploading(chips: Chip[], key: string, name: string, preview?: string): Chip[] {
  return [...chips, { key, name, state: "uploading", ...(preview ? { preview } : {}) }];
}

/** Marks a chip ready with what the daemon staged. A chip removed meanwhile stays removed. */
export function markReady(chips: Chip[], key: string, result: SessionAttachResult, preview?: string): Chip[] {
  return chips.map((chip) =>
    chip.key === key
      ? {
          ...chip,
          state: "ready",
          name: result.attachment.name,
          attachment: result.attachment,
          vision: result.vision,
          preview: preview ?? chip.preview,
        }
      : chip,
  );
}

export function markFailed(chips: Chip[], key: string, error: string): Chip[] {
  return chips.map((chip) => (chip.key === key ? { ...chip, state: "failed", error } : chip));
}

export function removeChip(chips: Chip[], key: string): Chip[] {
  return chips.filter((chip) => chip.key !== key);
}

/** The attachments to send: ready chips only, each with its thumbnail. */
export function readyAttachments(chips: Chip[]): Attachment[] {
  return chips.flatMap((chip) =>
    chip.state === "ready" && chip.attachment
      ? [{ ...chip.attachment, ...(chip.preview ? { preview: chip.preview } : {}) }]
      : [],
  );
}

/** Whether the composer can send: text or a ready attachment, and nothing still uploading. */
export function canSend(text: string, chips: Chip[]): boolean {
  if (chips.some((chip) => chip.state === "uploading")) return false;
  return text.trim().length > 0 || readyAttachments(chips).length > 0;
}

/** How the model receives an attachment, for its chip's tooltip. */
export function deliveryLabel(attachment: Pick<Attachment, "kind">, vision: boolean): string {
  switch (attachment.kind) {
    case "image":
      return vision
        ? "Sent as an image the model can see"
        : "This model cannot see images: sent as a file path the agent can read with its tools";
    case "pdf":
      return vision ? "Its text is extracted; its page images go to the model" : "Its text is extracted for the model";
    case "text":
      return "Its text is included in the message (long files are cut, with a marker)";
    default:
      return "Sent as a file path the agent can read with its tools";
  }
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The last component of a path, for a file dropped by path. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

/** A name for pasted content without one, such as a screenshot. */
export function pastedName(type: string, index: number): string {
  const extension = type.split("/")[1]?.split(/[+;]/)[0] || "bin";
  return `pasted-${index + 1}.${extension === "jpeg" ? "jpg" : extension}`;
}

// ---- Browser helpers ----

/** A file's bytes, base64 (no `data:` prefix). */
export function readBase64(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result);
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("the file could not be read"));
    reader.readAsDataURL(file);
  });
}

/** The largest side of a thumbnail, in pixels. */
const THUMB = 160;

/**
 * A small JPEG thumbnail of an image (a `data:` URL or a file), or undefined when it cannot be
 * drawn. Files are read as `data:` URLs, since the app's content policy allows no `blob:` images.
 */
export async function thumbnail(source: string | Blob): Promise<string | undefined> {
  try {
    const url = typeof source === "string" ? source : `data:${source.type};base64,${await readBase64(source)}`;
    const image = new Image();
    image.src = url;
    await image.decode();
    const scale = Math.min(1, THUMB / Math.max(image.naturalWidth, image.naturalHeight));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
    canvas.getContext("2d")?.drawImage(image, 0, 0, canvas.width, canvas.height);
    const preview = canvas.toDataURL("image/jpeg", 0.72);
    // The daemon keeps previews of at most 64 KiB.
    return preview.length <= 60 * 1024 ? preview : undefined;
  } catch {
    return undefined;
  }
}
