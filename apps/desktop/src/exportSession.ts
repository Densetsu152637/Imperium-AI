// Exporting a session's transcript (`session.export`, see docs/07-desktop-ui.md#exporting-transcripts):
// the call, and saving the result through the native save dialog in the desktop app or as a
// download in a browser build.

import { invoke, isTauri } from "@tauri-apps/api/core";

import type { ExportFormat, SessionExportParams, SessionExportResult } from "@imperium/protocol";

/** Anything that calls the daemon, such as the app's DaemonClient. */
export type ExportClient = { call<Result>(method: string, params?: unknown): Promise<Result> };

/** Asks the daemon for a session's transcript; running and archived sessions both work. */
export function requestExport(
  client: ExportClient,
  sessionId: string,
  format: ExportFormat,
  options: { includeSubagents?: boolean } = {},
): Promise<SessionExportResult> {
  const params: SessionExportParams = {
    session_id: sessionId,
    format,
    ...(options.includeSubagents === undefined ? {} : { include_subagents: options.includeSubagents }),
  };
  return client.call<SessionExportResult>("session.export", params);
}

/** The media type a download of `format` is labelled with. */
export function exportMediaType(format: ExportFormat): string {
  return format === "json" ? "application/json" : "text/markdown";
}

/** Saves an export: the save dialog in the desktop app, a download otherwise. False when cancelled. */
export async function saveExport(result: SessionExportResult): Promise<boolean> {
  if (isTauri()) {
    // The shell opens the save dialog and writes only the file picked there.
    return invoke<boolean>("save_export", { filename: result.filename, contents: result.content });
  }
  const url = URL.createObjectURL(new Blob([result.content], { type: exportMediaType(result.format) }));
  const link = document.createElement("a");
  link.href = url;
  link.download = result.filename;
  document.body.append(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
  return true;
}

/** Exports a session and saves it; false when the user cancelled the save. */
export async function exportSession(
  client: ExportClient,
  sessionId: string,
  format: ExportFormat,
): Promise<boolean> {
  const result = await requestExport(client, sessionId, format);
  // Exports are verbatim; when one looks like it holds a credential, the user decides.
  if (result.warning && !window.confirm(`${result.warning}

Save it anyway?`)) return false;
  return saveExport(result);
}
