// The form that adds a connector from the catalog.

import { KeyRound } from "lucide-react";
import { useId, useState } from "react";
import type { CatalogField, CatalogServer, McpSaveResult, McpServerInfo } from "@imperium/protocol";
import type { DaemonClient } from "../daemon/client";
import { message, NAME_PATTERN } from "./common";
import { FormShell, FormField } from "./FormParts";

const suggestName = (base: string, taken: Set<string>) => {
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
};

export function AddForm({
  entry,
  client,
  taken,
  onBack,
  onSaved,
}: {
  entry: CatalogServer;
  client: DaemonClient;
  taken: Set<string>;
  onBack: () => void;
  onSaved: (server: McpServerInfo) => void;
}) {
  const [name, setName] = useState(() => suggestName(entry.id, taken));
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(entry.fields.map((field) => [field.id, field.default ?? ""])),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const base = useId();

  const submit = async () => {
    if (!NAME_PATTERN.test(name)) return setError("Use lowercase letters, digits, '_' or '-' for the name.");
    const missing = entry.fields.find((field) => field.required && !values[field.id]?.trim());
    if (missing) return setError(`${missing.label} is required.`);
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<McpSaveResult>("mcp.install", { catalog_id: entry.id, name, values });
      onSaved(result.server);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  // What will run, with the form's values filled in (secrets stay hidden).
  const fill = (text: string) =>
    text.replace(/\{\{\s*([\w-]+)\s*\}\}/g, (_, id: string) => {
      const field = entry.fields.find((f) => f.id === id);
      if (!field) return "";
      if (field.kind === "secret") return "••••";
      return values[id]?.trim().split("\n").join(" ") || `<${field.label.toLowerCase()}>`;
    });
  const runs = entry.url ? `Connects to ${fill(entry.url)}` : `Runs ${fill([entry.command, ...entry.args].join(" "))}`;
  return (
    <FormShell
      title={`Add ${entry.name}`}
      icon={entry.icon}
      subtitle={
        <>
          {entry.description}
          {entry.homepage && (
            <>
              {" "}
              <a href={entry.homepage} target="_blank" rel="noreferrer">
                Learn more
              </a>
            </>
          )}
        </>
      }
      onBack={onBack}
      onSubmit={() => void submit()}
      submitLabel="Add connector"
      busy={busy}
      error={error}
    >
      <FormField label="Name" htmlFor={`${base}-name`} required description="Agents see its tools as mcp__name__tool.">
        <input id={`${base}-name`} className="field" value={name} onChange={(event) => setName(event.target.value.toLowerCase())} spellCheck={false} />
      </FormField>
      {entry.fields.map((field) => (
        <CatalogInput key={field.id} id={`${base}-${field.id}`} field={field} value={values[field.id] ?? ""} onChange={(value) => setValues((all) => ({ ...all, [field.id]: value }))} />
      ))}
      <p className="muted connector-runs">
        <code>{runs}</code>
      </p>
    </FormShell>
  );
}

function CatalogInput({ id, field, value, onChange }: { id: string; field: CatalogField; value: string; onChange: (value: string) => void }) {
  const description =
    field.kind === "secret" ? (
      <>
        <KeyRound aria-hidden="true" /> {field.description ? `${field.description} ` : ""}Saved to your system keychain; it is not
        shown again.
      </>
    ) : field.kind === "path_list" ? (
      // A catalogue description that already says "per line" is not said twice.
      field.description?.includes("per line") ? field.description : `${field.description ? `${field.description} ` : ""}One per line.`
    ) : (
      field.description
    );
  return (
    <FormField label={field.label} htmlFor={id} required={field.required} description={description}>
      {field.kind === "path_list" ? (
        <textarea id={id} className="field" rows={3} value={value} placeholder={field.placeholder ?? undefined} onChange={(event) => onChange(event.target.value)} spellCheck={false} />
      ) : (
        <input
          id={id}
          className="field"
          type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
          autoComplete={field.kind === "secret" ? "new-password" : "off"}
          value={value}
          placeholder={field.placeholder ?? undefined}
          onChange={(event) => onChange(event.target.value)}
          spellCheck={false}
        />
      )}
    </FormField>
  );
}
