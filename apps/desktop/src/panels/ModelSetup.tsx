// The first-run path to a usable model, shown while none is set up (the new-session composer and
// the Local models panel's empty state): add a folder of models already on disk, download one
// from Hugging Face, or connect a hosted API endpoint. Nothing here needs a config file edit or a
// daemon restart.

import { Cloud, Download, FolderPlus } from "lucide-react";
import { useState, type FormEvent } from "react";

import type { LocalModelListResult, ModelAddEndpointResult } from "@imperium/protocol";

import type { DaemonClient } from "../daemon/client";
import { pickFolder } from "../daemon/connection";

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/** Picks a folder and adds it to the model folders; resolves to the new listing, or null. */
export async function addModelFolder(client: DaemonClient): Promise<LocalModelListResult | null> {
  const path = await pickFolder("Add a model folder");
  if (!path) return null;
  return client.call<LocalModelListResult>("models.add_folder", { path });
}

export function ModelSetup({
  client,
  onChanged,
  onDownload,
  compact = false,
}: {
  client: DaemonClient | null;
  /** Called after a folder or endpoint was added, to refresh the model lists. */
  onChanged: () => void;
  /** Shows the Hugging Face download form. */
  onDownload: () => void;
  /** Leaves out the heading (when the surrounding panel already says it). */
  compact?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [endpointOpen, setEndpointOpen] = useState(false);

  const addFolder = async () => {
    if (!client) return;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const listing = await addModelFolder(client);
      if (listing) {
        const found = listing.models.length;
        setNote(
          found === 0
            ? "No models found in that folder yet — GGUF files and Hugging Face folders are recognised."
            : `Found ${found} model${found === 1 ? "" : "s"}. Pick one in Local models to test it and make it the default.`,
        );
        onChanged();
      }
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="model-setup" aria-labelledby={compact ? undefined : "model-setup-title"}>
      {!compact && (
        <>
          <h3 id="model-setup-title">Set up a model</h3>
          <p className="muted">Sessions need a model. Use models you already have, download one, or connect an API.</p>
        </>
      )}
      <div className="setup-actions">
        <button className="setup-action" onClick={addFolder} disabled={!client || busy}>
          <FolderPlus aria-hidden="true" />
          <span>
            <strong>Add a model folder</strong>
            <small>GGUF files, LM Studio or Hugging Face cache folders on this computer</small>
          </span>
        </button>
        <button className="setup-action" onClick={onDownload} disabled={!client}>
          <Download aria-hidden="true" />
          <span>
            <strong>Download from Hugging Face</strong>
            <small>Pick a quantisation that fits this machine</small>
          </span>
        </button>
        <button
          className="setup-action"
          onClick={() => setEndpointOpen((open) => !open)}
          aria-expanded={endpointOpen}
          disabled={!client}
        >
          <Cloud aria-hidden="true" />
          <span>
            <strong>Connect an API endpoint</strong>
            <small>OpenRouter, OpenAI, a server on your network — anything OpenAI-compatible</small>
          </span>
        </button>
      </div>
      {busy && (
        <p className="muted" role="status">
          Scanning…
        </p>
      )}
      {note && (
        <p className="muted" role="status">
          {note}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {endpointOpen && client && (
        <EndpointForm
          client={client}
          onAdded={(result) => {
            setEndpointOpen(false);
            setNote(
              result.default
                ? `Connected ${result.id}; new sessions use ${result.default}.`
                : `Connected ${result.id}. Use it as ${result.id}:<model>.`,
            );
            onChanged();
          }}
        />
      )}
    </section>
  );
}

function EndpointForm({ client, onAdded }: { client: DaemonClient; onAdded: (result: ModelAddEndpointResult) => void }) {
  const [id, setId] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<ModelAddEndpointResult>("models.add_endpoint", {
        id: id.trim(),
        base_url: baseUrl.trim(),
        api_key: apiKey || null,
        model: model.trim() || null,
        make_default: model.trim() !== "",
      });
      setApiKey("");
      onAdded(result);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="endpoint-form" onSubmit={submit} aria-label="Connect an API endpoint">
      <label>
        Name
        <input value={id} placeholder="openrouter" spellCheck={false} onChange={(e) => setId(e.target.value)} required />
      </label>
      <label>
        Base URL
        <input
          value={baseUrl}
          placeholder="https://openrouter.ai/api/v1"
          spellCheck={false}
          onChange={(e) => setBaseUrl(e.target.value)}
          required
        />
      </label>
      <label>
        API key
        <input
          type="password"
          value={apiKey}
          placeholder="kept in the system keychain"
          autoComplete="off"
          onChange={(e) => setApiKey(e.target.value)}
        />
      </label>
      <label>
        Model
        <input value={model} placeholder="openai/gpt-4.1-mini (optional)" spellCheck={false} onChange={(e) => setModel(e.target.value)} />
      </label>
      <p className="muted">With a model, it becomes the default for new sessions.</p>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <button type="submit" className="primary" disabled={busy || !id.trim() || !baseUrl.trim()}>
        Connect
      </button>
    </form>
  );
}
