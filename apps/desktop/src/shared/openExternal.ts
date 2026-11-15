// Opens a web address in the user's own browser. The desktop shell hands it to the system (only
// http and https addresses); in a plain browser tab it opens a new tab. Imperium never drives the
// browser it opens, and learns nothing from it.

import { invoke, isTauri } from "@tauri-apps/api/core";

export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    await invoke("open_external", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}
