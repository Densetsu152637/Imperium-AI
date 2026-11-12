// Settings → API: the OpenAI-compatible server that lets other tools (editors, chat front ends,
// scripts) use Imperium's local models. Off by default; it listens on this computer only unless
// the network is allowed, needs a token of its own, and serves hosted models (which spend your
// keys and money) only when each one is turned on here. Everything goes through `api.get`,
// `api.set` and `api.token`, which save to the daemon's config file and apply at once.

import { Copy } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

import type { ApiSetParams, ApiStatus, ApiTokenResult } from "@imperium/protocol";

import type { DaemonClient } from "../daemon/client";
import { Row, RowNote } from "./SettingsRow";
import { Switch } from "./Switch";

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/** What the API's settings change, for the call: absent fields stay as they are. */
const change = (part: Partial<ApiSetParams>): ApiSetParams => ({
  enabled: null,
  port: null,
  bind: null,
  allow_lan: null,
  expose: null,
  ...part,
});

export function ApiSection({ client }: { client: DaemonClient }) {
  const [status, setStatus] = useState<ApiStatus | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [port, setPort] = useState("");
  const [bind, setBind] = useState("");

  const adopt = useCallback((next: ApiStatus) => {
    setStatus(next);
    setPort(next.port ? String(next.port) : "");
    setBind(next.bind);
  }, []);

  useEffect(() => {
    let live = true;
    client
      .call<ApiStatus>("api.get", {})
      .then((next) => live && adopt(next))
      .catch((failure: unknown) => live && setError(message(failure)));
    return () => {
      live = false;
    };
  }, [client, adopt]);

  const apply = async (part: Partial<ApiSetParams>) => {
    setBusy(true);
    setError(null);
    try {
      adopt(await client.call<ApiStatus>("api.set", change(part)));
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const fetchToken = async (rotate: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<ApiTokenResult>("api.token", { rotate });
      setToken(result.token);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (what: string, text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(what);
      window.setTimeout(() => setCopied((current) => (current === what ? null : current)), 2000);
    } catch (failure) {
      setError(`Could not copy: ${message(failure)}`);
    }
  };

  if (!status) return error ? <p className="error">{error}</p> : <p className="muted">Loading…</p>;

  const local = status.models.filter((model) => !model.hosted);
  const hosted = status.models.filter((model) => model.hosted);
  const portChanged = port.trim() !== (status.port ? String(status.port) : "");
  const bindChanged = bind.trim() !== status.bind;

  return (
    <>
      <Row
        label="OpenAI-compatible API"
        description="Lets other tools on this computer use your local models through an OpenAI-style API: set their base URL and API key below. Calls load models and queue like any other, and count toward budgets."
      >
        <Switch
          checked={status.enabled}
          onChange={(enabled) => void apply({ enabled })}
          label="OpenAI-compatible API"
          disabled={busy}
        />
      </Row>
      {status.error && (
        <p className="error" role="alert">
          The API is not running: {status.error}
        </p>
      )}
      {status.running && status.base_url && (
        <>
          <Row label="Base URL" description="Where tools send requests (/models and /chat/completions).">
            <code>{status.base_url}</code>
            <button type="button" onClick={() => void copy("url", status.base_url ?? "")}>
              <Copy aria-hidden="true" /> {copied === "url" ? "Copied" : "Copy"}
            </button>
          </Row>
          <Row
            label="API key"
            description="Tools send this token as their API key. It is kept in your keychain, separate from the daemon's, and rotating it ends every tool's access until you give it the new one."
          >
            {token ? (
              <>
                <code className="api-token">{token}</code>
                <button type="button" onClick={() => void copy("token", token)}>
                  <Copy aria-hidden="true" /> {copied === "token" ? "Copied" : "Copy"}
                </button>
                <button type="button" onClick={() => setToken(null)}>
                  Hide
                </button>
              </>
            ) : (
              <button type="button" disabled={busy} onClick={() => void fetchToken(false)}>
                Show token
              </button>
            )}
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                if (window.confirm("Rotate the API token? Tools using the old one stop working until you update them.")) {
                  void fetchToken(true);
                }
              }}
            >
              Rotate
            </button>
          </Row>
        </>
      )}
      <Row
        label="Port"
        description="Chosen for you the first time the API is turned on, and kept so tools need setting up once. Changing it restarts the API."
      >
        <input
          className="field budget-number"
          type="number"
          min={1}
          max={65535}
          aria-label="API port"
          value={port}
          disabled={busy}
          onChange={(event) => setPort(event.target.value)}
        />
        <button
          type="button"
          disabled={busy || !portChanged || !Number.isInteger(Number(port)) || Number(port) <= 0}
          onClick={() => void apply({ port: Number(port) })}
        >
          Apply
        </button>
      </Row>
      <Row
        label="Allow other computers"
        description="Listens beyond this computer, over plain HTTP. Risky: anyone on the network who learns the token can use your models, and the hosted ones you turn on below. Leave it off unless the network is trusted."
      >
        <Switch
          checked={status.allow_lan}
          onChange={(allow_lan) => void apply(allow_lan ? { allow_lan } : { allow_lan, bind: "127.0.0.1" })}
          label="Allow other computers"
          disabled={busy}
        />
      </Row>
      {status.allow_lan && (
        <Row sub label="Address" description="The address to listen on, such as 0.0.0.0 for every network interface.">
          <input
            className="field"
            aria-label="API address"
            spellCheck={false}
            value={bind}
            disabled={busy}
            onChange={(event) => setBind(event.target.value)}
          />
          <button type="button" disabled={busy || !bindChanged || !bind.trim()} onClick={() => void apply({ bind: bind.trim() })}>
            Apply
          </button>
        </Row>
      )}

      <section className="environment-section" aria-label="Models served">
        <div className="environment-section-head">
          <span className="environment-label">Models served</span>
        </div>
        {local.length === 0 ? (
          <p className="muted">No local models found. Add a model folder in Settings → Models.</p>
        ) : (
          <ul className="muted">
            {local.map((model) => (
              <li key={model.id}>
                {model.name} · <code>{model.id}</code>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="environment-section" aria-label="Hosted models">
        <div className="environment-section-head">
          <span className="environment-label">Hosted models</span>
        </div>
        <p className="muted permissions-hint">
          Off by default. A hosted model that is on is used with your key and billed to you by its provider, by anything that has
          the token. Rows of the model table appear here; turn on only what you mean to share.
        </p>
        {hosted.length === 0 && <p className="muted">No hosted models in the model table.</p>}
        {hosted.map((model) => (
          <Row key={model.id} label={model.name} description={<code>{model.id}</code>}>
            <Switch
              checked={model.exposed}
              disabled={busy}
              label={`Serve ${model.name}`}
              onChange={(on) =>
                void apply({
                  expose: on ? [...new Set([...status.expose, model.id])] : status.expose.filter((spec) => spec !== model.id),
                })
              }
            />
          </Row>
        ))}
      </section>
      <RowNote>
        To cap what other tools use, add a budget with the scope “API calls” in Settings → Budgets. A budget that is used up
        refuses API calls with an error the tool can show, even one set to ask first.
      </RowNote>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </>
  );
}
