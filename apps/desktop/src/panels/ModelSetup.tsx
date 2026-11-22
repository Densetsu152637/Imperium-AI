// The first-run path to a usable model, shown while none is set up (the new-session composer and
// the Local models panel's empty state): add a folder of models already on disk, download one
// from Hugging Face, or connect a hosted API endpoint. Nothing here needs a config file edit or a
// daemon restart.

import { Cloud, Download, FolderPlus, type LucideIcon } from "lucide-react";
import { useId, useState, type FormEvent, type ReactNode } from "react";

import type { LocalModelListResult, ModelAddEndpointResult } from "@imperium/protocol";

import { Field } from "../components/Field";
import { Notice } from "../components/Notice";
import { Spinner } from "../components/LoadingState";
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
        <SetupCard icon={FolderPlus} title="Add a model folder" body="GGUF files, LM Studio or Hugging Face cache folders on this computer">
          <button className="setup-action" onClick={addFolder} disabled={!client || busy}>
            Add folder
          </button>
        </SetupCard>
        <SetupCard icon={Download} title="Download from Hugging Face" body="Pick a quantisation that fits this machine">
          <button className="setup-action" onClick={onDownload} disabled={!client}>
            Download
          </button>
        </SetupCard>
        <SetupCard
          icon={Cloud}
          title="Connect an API endpoint"
          body="OpenRouter, OpenAI, a server on your network — anything OpenAI-compatible"
        >
          <button
            className="setup-action"
            onClick={() => setEndpointOpen((open) => !open)}
            aria-expanded={endpointOpen}
            disabled={!client}
          >
            Connect
          </button>
        </SetupCard>
      </div>
      {busy && (
        <p className="muted setup-status" role="status">
          <Spinner /> Scanning…
        </p>
      )}
      {note && (
        <p className="muted setup-status" role="status">
          {note}
        </p>
      )}
      {error && <Notice tone="danger">{error}</Notice>}
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

/** One of the three choices: an icon, what it does, and one large action. */
function SetupCard({ icon: Icon, title, body, children }: { icon: LucideIcon; title: string; body: string; children: ReactNode }) {
  const titleId = useId();
  return (
    <div className="card setup-card" role="group" aria-labelledby={titleId}>
      <Icon className="setup-icon" aria-hidden="true" />
      <div className="setup-text">
        <strong id={titleId}>{title}</strong>
        <span className="muted">{body}</span>
      </div>
      {children}
    </div>
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
      <Field label="Name">
        <input value={id} placeholder="openrouter" spellCheck={false} onChange={(e) => setId(e.target.value)} required />
      </Field>
      <Field label="Base URL">
        <input
          value={baseUrl}
          placeholder="https://openrouter.ai/api/v1"
          spellCheck={false}
          onChange={(e) => setBaseUrl(e.target.value)}
          required
        />
      </Field>
      <Field label="API key">
        <input
          type="password"
          value={apiKey}
          placeholder="kept in the system keychain"
          autoComplete="off"
          onChange={(e) => setApiKey(e.target.value)}
        />
      </Field>
      <Field label="Model">
        <input value={model} placeholder="openai/gpt-4.1-mini (optional)" spellCheck={false} onChange={(e) => setModel(e.target.value)} />
      </Field>
      <p className="muted">With a model, it becomes the default for new sessions.</p>
      {error && <Notice tone="danger">{error}</Notice>}
      <button type="submit" className="primary" disabled={busy || !id.trim() || !baseUrl.trim()}>
        Connect
      </button>
    </form>
  );
}
