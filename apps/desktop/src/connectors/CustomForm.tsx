// The form that adds an MCP server of the user's own.

import { KeyRound, Plus, X } from "lucide-react";
import { useId, useState } from "react";
import type { McpSaveResult, McpServerConfig, McpServerInfo } from "@imperium/protocol";
import { Field } from "../components/Field";
import { IconButton } from "../components/IconButton";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";
import { message, NAME_PATTERN } from "./common";
import { FormShell, FieldGroup, focusField } from "./FormParts";

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
  // Problems with one field, by field key (name, command, url, secrets, timeout), shown under that field.
  const [problems, setProblems] = useState<Record<string, string>>({});
  const base = useId();
  const clear = (key: string) => setProblems((all) => (key in all ? Object.fromEntries(Object.entries(all).filter(([k]) => k !== key)) : all));

  const submit = async () => {
    const found: Record<string, string> = {};
    if (!NAME_PATTERN.test(name)) found.name = "Use lowercase letters, digits, '_' or '-' for the name.";
    else if (!editing && taken.has(name)) found.name = `A connector named ${name} already exists.`;
    if (transport === "stdio" && !command.trim()) found.command = "Enter the command that starts the server.";
    if (transport === "http" && !url.trim()) found.url = "Enter the server's URL.";
    const missingSecret = transport === "stdio" ? secretRows.findIndex((row) => row.key.trim() && !row.value && !row.existing) : -1;
    if (missingSecret >= 0) found.secrets = `Enter a value for ${secretRows[missingSecret].key.trim()}.`;
    const seconds = timeout.trim() ? Number(timeout) : null;
    if (seconds !== null && !(Number.isInteger(seconds) && seconds > 0)) found.timeout = "The timeout is a whole number of seconds.";
    setProblems(found);
    const first = ["name", "command", "url", "secrets", "timeout"].find((key) => key in found);
    if (first) return focusField(first === "secrets" ? `${base}-secret-${missingSecret}` : `${base}-${first}`);
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
        <Field label="Name" required error={problems.name}>
          <input
            id={`${base}-name`}
            value={name}
            disabled={Boolean(editing)}
            onChange={(event) => {
              setName(event.target.value.toLowerCase());
              clear("name");
            }}
            placeholder="my-server"
            spellCheck={false}
          />
        </Field>
        <Field label="Type">
          <Select
            value={transport}
            onChange={(value) => setTransport(value as "stdio" | "http")}
            options={[
              { value: "stdio", label: "Local command", description: "A process speaking MCP on stdin/stdout" },
              { value: "http", label: "Remote URL", description: "A streamable-HTTP MCP endpoint" },
            ]}
          />
        </Field>
      </div>
      {transport === "stdio" ? (
        <>
          <Field label="Command" required error={problems.command}>
            <input
              id={`${base}-command`}
              value={command}
              onChange={(event) => {
                setCommand(event.target.value);
                clear("command");
              }}
              placeholder="npx"
              spellCheck={false}
            />
          </Field>
          <Field label="Arguments" hint="One per line.">
            <textarea rows={3} value={args} onChange={(event) => setArgs(event.target.value)} placeholder={"-y\n@scope/server"} spellCheck={false} />
          </Field>
          <Field label="Environment" hint="KEY=value, one per line. Put tokens under secret variables instead.">
            <textarea rows={2} value={env} onChange={(event) => setEnv(event.target.value)} spellCheck={false} />
          </Field>
          <FieldGroup
            id={`${base}-secrets`}
            label="Secret variables"
            error={problems.secrets}
            hint={
              <>
                <KeyRound aria-hidden="true" /> Values are saved to your system keychain and never shown again.
              </>
            }
          >
            {secretRows.map((row, index) => (
              <div key={index} className="secret-row">
                <input
                  className="field"
                  id={`${base}-secret-${index}`}
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
                <IconButton
                  icon={X}
                  label={`Remove ${row.key || "variable"}`}
                  onClick={() => {
                    setSecretRows((all) => all.filter((_, i) => i !== index));
                    clear("secrets");
                  }}
                />
              </div>
            ))}
            <button type="button" className="small add-secret" onClick={() => setSecretRows((all) => [...all, { key: "", value: "", existing: null }])}>
              <Plus aria-hidden="true" /> Add secret variable
            </button>
          </FieldGroup>
        </>
      ) : (
        <>
          <Field label="URL" required error={problems.url}>
            <input
              id={`${base}-url`}
              type="url"
              value={url}
              onChange={(event) => {
                setUrl(event.target.value);
                clear("url");
              }}
              placeholder="https://example.com/mcp"
              spellCheck={false}
            />
          </Field>
          <Field
            label="Bearer token"
            hint={
              <>
                <KeyRound aria-hidden="true" /> Sent as Authorization: Bearer. Saved to your system keychain.
              </>
            }
          >
            <input
              type="password"
              autoComplete="new-password"
              value={bearer}
              disabled={clearBearer}
              placeholder={config?.bearer_secret ? "Saved · type to replace" : "optional"}
              onChange={(event) => setBearer(event.target.value)}
            />
          </Field>
          {config?.bearer_secret && (
            <label className="inline-check">
              <input type="checkbox" checked={clearBearer} onChange={(event) => setClearBearer(event.target.checked)} /> Don't send the saved token
            </label>
          )}
          <Field label="Headers" hint="Name: value, one per line.">
            <textarea rows={2} value={headers} onChange={(event) => setHeaders(event.target.value)} spellCheck={false} />
          </Field>
        </>
      )}
      <div className="form-grid">
        <Field label="Tools that run without asking" hint="Tool names, one per line, or * for all.">
          <textarea rows={2} value={allow} onChange={(event) => setAllow(event.target.value)} spellCheck={false} />
        </Field>
        <Field label="Timeout (seconds)" hint="Per call; 60 when empty." error={problems.timeout}>
          <input
            id={`${base}-timeout`}
            type="number"
            min={1}
            value={timeout}
            onChange={(event) => {
              setTimeoutSeconds(event.target.value);
              clear("timeout");
            }}
          />
        </Field>
      </div>
    </FormShell>
  );
}
