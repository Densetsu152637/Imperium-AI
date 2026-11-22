// First-run suggestion of the house models (Emperor and Consul), shown above the model setup
// while the user has no model at all. A model without a download source shows as still in
// training; one with a source downloads through the same daemon calls as the Hugging Face form.
// The card can be dismissed, and stays dismissed (a setting).

import { X } from "lucide-react";
import { useState } from "react";

import type { DownloadJob, HfFilesResult } from "@imperium/protocol";

import { Badge } from "../components/Badge";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import type { DaemonClient } from "../daemon/client";
import { approxSize, downloadable, type HouseModel } from "../models/houseModels";

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

export function HouseModelCard({
  models,
  client,
  onDismiss,
  onStarted,
}: {
  models: HouseModel[];
  client: DaemonClient | null;
  onDismiss: () => void;
  /** Called after a download started, so the model lists and downloads can be followed. */
  onStarted: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [started, setStarted] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);

  if (models.length === 0) return null;

  const download = async (model: HouseModel) => {
    if (!client || !downloadable(model)) return;
    setBusy(model.id);
    setError(null);
    try {
      const files = await client.call<HfFilesResult>("models.hf_files", { repo: model.source.repo });
      await client.call<DownloadJob>("models.download", {
        repo: files.repo,
        revision: files.revision,
        quant: model.source.quant ?? files.default ?? files.variants[0]?.name ?? null,
        mmproj: false,
      });
      setStarted((current) => ({ ...current, [model.id]: true }));
      onStarted();
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="card house-models" aria-labelledby="house-models-title">
      <div className="house-models-head">
        <h3 id="house-models-title">Imperium's own models</h3>
        <IconButton icon={X} label="Dismiss the model suggestion" tooltip="Don't suggest these again" onClick={onDismiss} />
      </div>
      <p className="muted">No model is set up yet. Emperor and Consul are the models Imperium is built around.</p>
      <table className="table house-model-table">
        <thead>
          <tr>
            <th>Model</th>
            <th className="num">Size</th>
            <th>
              <span className="visually-hidden">Action</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {models.map((model) => (
            <tr key={model.id}>
              <td>
                <span className="house-model-text">
                  <strong>{model.name}</strong>
                  <span className="muted">{model.role}</span>
                </span>
              </td>
              <td className="num">{approxSize(model.approxBytes)}</td>
              <td className="house-model-action">
                {downloadable(model) ? (
                  <button
                    className="primary"
                    disabled={!client || busy !== null || started[model.id]}
                    onClick={() => download(model)}
                  >
                    {started[model.id] ? "Downloading" : `Download ${model.name}`}
                  </button>
                ) : (
                  <Badge>Not yet available: still in training</Badge>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {error && (
        <Notice tone="danger">
          {error}
        </Notice>
      )}
    </section>
  );
}
