// Changes agents made to the app config (`config.changes`) and where installed extensions came
// from (`installed_from`), as Settings → Changes by agents and the Connectors window show them.

import type { ConfigChange, InstallSource } from "@imperium/protocol";

/** Where an extension came from, in one line: the URL and short commit, or the folder. */
export function sourceLabel(source: InstallSource): string {
  let label = source.location;
  if (source.subdir) label += ` (folder ${source.subdir})`;
  if (source.revision) label += ` at ${source.revision.slice(0, 12)}`;
  return label;
}

/** The details under a change's summary: the card before and after, or the folder and source. */
export function changeDetails(change: ConfigChange): string[] {
  const details: string[] = [];
  if (change.kind === "model_card") {
    details.push(`Before: ${cardLine(change.before)}`);
    details.push(`After: ${cardLine(change.after)}`);
  }
  if (change.source) details.push(`From ${sourceLabel(change.source)}`);
  if (change.path) details.push(change.path);
  if (change.disabled) details.push("Installed turned off, since it has hooks; turn it on in Connectors.");
  return details;
}

/** What undoing a change does, for its button's tooltip. */
export function undoLabel(change: ConfigChange): string {
  switch (change.kind) {
    case "model_card":
      return change.before ? "Put the earlier card back" : "Remove this card";
    case "install":
      return `Uninstall the ${change.extension_kind ?? "extension"}`;
    default:
      return `Restore the ${change.extension_kind ?? "extension"}`;
  }
}

function cardLine(card: string | null): string {
  if (!card) return "none";
  const flat = card.split(/\s+/).filter(Boolean).join(" ");
  return flat.length > 160 ? `${flat.slice(0, 160)}…` : flat;
}

/** Whether an agent installed the extension (its source names a session). */
export function installedByAgent(item: { installed_from?: InstallSource | null }): boolean {
  return Boolean(item.installed_from?.session_id);
}
