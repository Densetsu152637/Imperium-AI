// The form that adds an MCP server of the user's own.

import { KeyRound, Plus, X } from "lucide-react";
import { useId, useState } from "react";
import type { McpSaveResult, McpServerConfig, McpServerInfo } from "@imperium/protocol";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";
import { message, NAME_PATTERN } from "./common";
import { FormShell, FormField } from "./FormParts";

/** A secret environment variable being edited: an existing secret is kept unless a value is typed. */
type SecretRow = { key: string; value: string; existing: string | null };

const lines = (text: string) =>
  text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

/** `KEY=value` (or `Name: value` for headers) lines to a map. */
function pairs(text: string, separator: "=" | ":"): Record<string, string> {
  return Object.fromEntries(
    lines(text).flatMap((line) => {
      const at = line.indexOf(separator);
      return at > 0 ? [[line.slice(0, at).trim(), line.slice(at + 1).trim()]] : [];
    }),
  );
}

const unpairs = (map: Record<string, string>, separator: string) =>
  Object.entries(map)
    .map(([key, value]) => `${key}${separator}${value}`)
    .join("\n");

/** A keychain name for one of a server's secrets, within the 64-character limit. */
const secretName = (server: string, key: string) =>
  `mcp.${server}.${key.toLowerCase().replace(/[^a-z0-9.-]+/g, "-").replace(/^-+|-+$/g, "") || "secret"}`.slice(0, 64);

export function CustomForm({
  editing,
  client,
  taken,
  onBack,
  onSaved,
}: {
  editing: McpServerInfo | null;
  client: DaemonClient;
  taken: Set<string>;
  onBack: () => void;
  onSaved: (server: McpServerInfo) => void;
}) {
  const config = editing?.config;
  const [name, setName] = useState(editing?.name ?? "");
  const [transport, setTransport] = useState<"stdio" | "http">(config?.url ? "http" : "stdio");
  const [command, setCommand] = useState(config?.command ?? "");
  const [args, setArgs] = useState(config?.args.join("\n") ?? "");
  const [env, setEnv] = useState(config ? unpairs(config.env, "=") : "");
  const [secretRows, setSecretRows] = useState<SecretRow[]>(
    Object.entries(config?.secret_env ?? {}).map(([key, existing]) => ({ key, value: "", existing })),
  );
  const [url, setUrl] = useState(config?.url ?? "");
  const [headers, setHeaders] = useState(config ? unpairs(config.headers, ": ") : "");
  const [bearer, setBearer] = useState("");
  const [clearBearer, setClearBearer] = useState(false);
  const [allow, setAllow] = useState(config?.allow.join("\n") ?? "");
  const [timeout, setTimeoutSeconds] = useState(config?.timeout_s?.toString() ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const base = useId();

  const submit = async () => {
    if (!NAME_PATTERN.test(name)) return setError("Use lowercase letters, digits, '_' or '-' for the name.");
    if (!editing && taken.has(name)) return setError(`A connector named ${name} already exists.`);
    if (transport === "stdio" && !command.trim()) return setError("Enter the command that starts the server.");
    if (transport === "http" && !url.trim()) return setError("Enter the server's URL.");
    const secrets: Record<string, string> = {};
    const secretEnv: Record<string, string> = {};
    if (transport === "stdio") {
      for (const row of secretRows) {
        const key = row.key.trim();
        if (!key) continue;
        if (row.value) {
          const secret = row.existing ?? secretName(name, key);
          secrets[secret] = row.value;
          secretEnv[key] = secret;
        } else if (row.existing) {
          secretEnv[key] = row.existing;
        } else {
          return setError(`Enter a value for ${key}.`);
        }
      }
    }
    let bearerSecret: string | null = null;
    if (transport === "http" && !clearBearer) {
      if (bearer) {
        bearerSecret = config?.bearer_secret ?? secretName(name, "token");
        secrets[bearerSecret] = bearer;
      } else {
        bearerSecret = config?.bearer_secret ?? null;
      }
    }
    const seconds = timeout.trim() ? Number(timeout) : null;
    if (seconds !== null && !(Number.isInteger(seconds) && seconds > 0)) return setError("The timeout is a whole number of seconds.");
    const next: McpServerConfig = {
      command: transport === "stdio" ? command.trim() : null,
      args: transport === "stdio" ? lines(args) : [],
      env: transport === "stdio" ? pairs(env, "=") : {},
      secret_env: secretEnv,
      url: transport === "http" ? url.trim() : null,
      headers: transport === "http" ? pairs(headers, ":") : {},
      bearer_secret: bearerSecret,
      allow: lines(allow),
      enabled: config?.enabled ?? true,
      timeout_s: seconds,
      catalog: config?.catalog ?? null,
    };
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<McpSaveResult>("mcp.save", { name, config: next, secrets, create: !editing });
      onSaved(result.server);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <FormShell
      title={editing ? `Configure ${editing.name}` : "Custom connector"}
      icon={transport === "http" ? "cloud" : "terminal"}
      subtitle="Saved to [mcp.servers] in your config file. Secrets go to your system keychain."
      onBack={onBack}
      onSubmit={() => void submit()}
      submitLabel={editing ? "Save" : "Add connector"}
      busy={busy}
      error={error}
    >
      <div className="form-grid">
        <FormField label="Name" htmlFor={`${base}-name`} required>
          <input
            id={`${base}-name`}
            className="field"
            value={name}
            disabled={Boolean(editing)}
            onChange={(event) => setName(event.target.value.toLowerCase())}
            placeholder="my-server"
            spellCheck={false}
          />
        </FormField>
        <FormField label="Type" htmlFor={`${base}-type`}>
          <Select
            id={`${base}-type`}
            value={transport}
            onChange={(value) => setTransport(value as "stdio" | "http")}
            options={[
              { value: "stdio", label: "Local command", description: "A process speaking MCP on stdin/stdout" },
              { value: "http", label: "Remote URL", description: "A streamable-HTTP MCP endpoint" },
            ]}
          />
        </FormField>
      </div>
      {transport === "stdio" ? (
        <>
          <FormField label="Command" htmlFor={`${base}-command`} required>
            <input id={`${base}-command`} className="field" value={command} onChange={(event) => setCommand(event.target.value)} placeholder="npx" spellCheck={false} />
          </FormField>
          <FormField label="Arguments" htmlFor={`${base}-args`} description="One per line.">
            <textarea id={`${base}-args`} className="field" rows={3} value={args} onChange={(event) => setArgs(event.target.value)} placeholder={"-y\n@scope/server"} spellCheck={false} />
          </FormField>
          <FormField label="Environment" htmlFor={`${base}-env`} description="KEY=value, one per line. Put tokens under secret variables instead.">
            <textarea id={`${base}-env`} className="field" rows={2} value={env} onChange={(event) => setEnv(event.target.value)} spellCheck={false} />
          </FormField>
          <div className="form-field">
            <span className="form-field-label">Secret variables</span>
            {secretRows.map((row, index) => (
              <div key={index} className="secret-row">
                <input
                  className="field"
                  aria-label="Variable name"
                  placeholder="API_TOKEN"
                  value={row.key}
                  disabled={row.existing !== null}
                  onChange={(event) => setSecretRows((all) => all.map((r, i) => (i === index ? { ...r, key: event.target.value } : r)))}
                  spellCheck={false}
                />
                <input
                  className="field"
                  type="password"
                  autoComplete="new-password"
                  aria-label={`Value of ${row.key || "the variable"}`}
                  placeholder={row.existing ? "Saved · type to replace" : "value"}
                  value={row.value}
                  onChange={(event) => setSecretRows((all) => all.map((r, i) => (i === index ? { ...r, value: event.target.value } : r)))}
                />
                <button
                  type="button"
                  className="ghost icon-only"
                  aria-label={`Remove ${row.key || "variable"}`}
                  onClick={() => setSecretRows((all) => all.filter((_, i) => i !== index))}
                >
                  <X />
                </button>
              </div>
            ))}
            <button type="button" className="small add-secret" onClick={() => setSecretRows((all) => [...all, { key: "", value: "", existing: null }])}>
              <Plus aria-hidden="true" /> Add secret variable
            </button>
            <span className="form-field-description">
              <KeyRound aria-hidden="true" /> Values are saved to your system keychain and never shown again.
            </span>
          </div>
        </>
      ) : (
        <>
          <FormField label="URL" htmlFor={`${base}-url`} required>
            <input id={`${base}-url`} className="field" type="url" value={url} onChange={(event) => setUrl(event.target.value)} placeholder="https://example.com/mcp" spellCheck={false} />
          </FormField>
          <FormField
            label="Bearer token"
            htmlFor={`${base}-bearer`}
            description={
              <>
                <KeyRound aria-hidden="true" /> Sent as Authorization: Bearer. Saved to your system keychain.
              </>
            }
          >
            <div className="secret-row">
              <input
                id={`${base}-bearer`}
                className="field"
                type="password"
                autoComplete="new-password"
                value={bearer}
                disabled={clearBearer}
                placeholder={config?.bearer_secret ? "Saved · type to replace" : "optional"}
                onChange={(event) => setBearer(event.target.value)}
              />
              {config?.bearer_secret && (
                <label className="inline-check">
                  <input type="checkbox" checked={clearBearer} onChange={(event) => setClearBearer(event.target.checked)} /> Don't send
                </label>
              )}
            </div>
          </FormField>
          <FormField label="Headers" htmlFor={`${base}-headers`} description="Name: value, one per line.">
            <textarea id={`${base}-headers`} className="field" rows={2} value={headers} onChange={(event) => setHeaders(event.target.value)} spellCheck={false} />
          </FormField>
        </>
      )}
      <div className="form-grid">
        <FormField label="Tools that run without asking" htmlFor={`${base}-allow`} description="Tool names, one per line, or * for all.">
          <textarea id={`${base}-allow`} className="field" rows={2} value={allow} onChange={(event) => setAllow(event.target.value)} spellCheck={false} />
        </FormField>
        <FormField label="Timeout (seconds)" htmlFor={`${base}-timeout`} description="Per call; 60 when empty.">
          <input id={`${base}-timeout`} className="field" type="number" min={1} value={timeout} onChange={(event) => setTimeoutSeconds(event.target.value)} />
        </FormField>
      </div>
    </FormShell>
  );
}
