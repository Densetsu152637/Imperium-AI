// Local models: GGUF files in the model folders, the hardware they are planned against, and
// which are loaded.

import type { LocalModelListResult, LocalModelRow } from "@imperium/protocol";

const GIB = 1024 ** 3;

const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;

function fit(row: LocalModelRow): string {
  if (row.loaded) return `loaded · ${row.slots_in_use}/${row.slots} busy`;
  switch (row.placement) {
    case "gpu":
      return "fits on GPU";
    case "partial":
      return `partial · ${row.gpu_layers ?? 0} GPU layers`;
    case "cpu":
      return "fits on CPU";
    case "none":
      return "does not fit";
  }
}

export function LocalModelsPanel({
  listing,
  onRefresh,
}: {
  listing: LocalModelListResult | null;
  onRefresh: () => void;
}) {
  if (!listing || !listing.enabled) {
    return (
      <p className="empty">
        No model folders yet. Add <code>folders = ["D:/Models"]</code> under <code>[models]</code> in your config, then
        restart the daemon.
      </p>
    );
  }
  const { hardware, runtime } = listing;
  return (
    <div className="models local-models">
      <div className="models-toolbar">
        {hardware && (
          <span className="muted">
            {hardware.gpus.map((gpu) => `${gpu.name} ${gib(gpu.vram_free_bytes)} free`).join(" · ") || "no GPU"} · RAM{" "}
            {gib(hardware.ram_available_bytes)} free{hardware.overridden ? " (overridden)" : ""}
          </span>
        )}
        <button onClick={onRefresh}>Refresh</button>
      </div>
      {runtime && !runtime.installed && (
        <p className="warning">
          llama.cpp pack <code>{runtime.pack}</code> is not installed. Run <code>imperium runtime install</code>.
        </p>
      )}
      <table>
        <thead>
          <tr>
            <th>Id</th>
            <th>Quant</th>
            <th>Size</th>
            <th>Needs</th>
            <th>Placement</th>
          </tr>
        </thead>
        <tbody>
          {listing.models.map((row) => (
            <tr key={row.id} className={row.placement === "none" && !row.loaded ? "unusable" : undefined} title={row.path}>
              <td>{`${row.runtime}:${row.id}`}</td>
              <td>{row.quantization ?? "?"}</td>
              <td>{gib(row.size_bytes)}</td>
              <td>{gib(row.required_bytes)}</td>
              <td>
                <span className={`badge ${row.loaded ? "local" : ""}`}>{fit(row)}</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {listing.models.length === 0 && <p className="empty">No GGUF models in {listing.folders.join(", ")}.</p>}
      {listing.skipped.length > 0 && (
        <p className="muted">
          Skipped {listing.skipped.length} file{listing.skipped.length === 1 ? "" : "s"}:{" "}
          {listing.skipped.map((s) => `${s.path} (${s.reason})`).join("; ")}
        </p>
      )}
    </div>
  );
}
