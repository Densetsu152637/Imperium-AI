// Attachment chips: in the composer (removable, with upload state) and in the transcript (what a
// sent message carried). Images show their thumbnail; other files an icon.

import { File, FileText, Image as ImageIcon, LoaderCircle, TriangleAlert, X } from "lucide-react";

import type { Attachment } from "@imperium/protocol";

import { deliveryLabel, formatBytes, type Chip } from "../attachments";

function KindIcon({ kind }: { kind: Attachment["kind"] | undefined }) {
  if (kind === "image") return <ImageIcon aria-hidden="true" />;
  if (kind === "text" || kind === "pdf") return <FileText aria-hidden="true" />;
  return <File aria-hidden="true" />;
}

/** The composer's chips; each can be removed before sending. */
export function ComposerChips({ chips, onRemove }: { chips: Chip[]; onRemove: (key: string) => void }) {
  if (chips.length === 0) return null;
  return (
    <ul className="attachment-chips" aria-label="Attachments">
      {chips.map((chip) => {
        const title =
          chip.state === "failed"
            ? chip.error
            : chip.attachment
              ? `${chip.attachment.path}\n${deliveryLabel(chip.attachment, chip.vision ?? false)}`
              : "Uploading…";
        const noVision = chip.attachment?.kind === "image" && !chip.vision;
        return (
          <li key={chip.key} className={`attachment-chip ${chip.state}`} title={title}>
            {chip.preview ? <img src={chip.preview} alt="" /> : <KindIcon kind={chip.attachment?.kind} />}
            <span className="attachment-name">{chip.name}</span>
            {chip.state === "uploading" && <LoaderCircle className="spin" aria-label="Uploading" />}
            {chip.state === "failed" && <TriangleAlert aria-label={chip.error ?? "Failed"} />}
            {chip.state === "ready" && chip.attachment && (
              <span className="attachment-size muted">
                {formatBytes(chip.attachment.size)}
                {noVision && " · path only"}
              </span>
            )}
            <button
              type="button"
              className="ghost icon-only"
              onClick={() => onRemove(chip.key)}
              aria-label={`Remove ${chip.name}`}
              title="Remove"
            >
              <X aria-hidden="true" />
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/** What a sent message carried, in the transcript. */
export function SentAttachments({ attachments }: { attachments: Attachment[] }) {
  return (
    <ul className="attachment-chips sent" aria-label="Attachments">
      {attachments.map((attachment) => (
        <li
          key={attachment.id}
          className="attachment-chip ready"
          title={`${attachment.path}\n${deliveryLabel(attachment, attachment.vision ?? false)}`}
        >
          {attachment.preview ? <img src={attachment.preview} alt="" /> : <KindIcon kind={attachment.kind} />}
          <span className="attachment-name">{attachment.name}</span>
          <span className="attachment-size muted">{formatBytes(attachment.size)}</span>
        </li>
      ))}
    </ul>
  );
}
