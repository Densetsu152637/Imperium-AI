// The form that adds a connector from the catalog.

import { KeyRound } from "lucide-react";
import { useId, useState } from "react";
import type { CatalogField, CatalogServer, McpSaveResult, McpServerInfo } from "@imperium/protocol";
import type { DaemonClient } from "../daemon/client";
import { Field } from "../components/Field";
import { message, NAME_PATTERN } from "./common";
import { FormShell, focusField } from "./FormParts";

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
  // Problems with one field, by field id ("name" for the name), shown under that field.
  const [problems, setProblems] = useState<Record<string, string>>({});
  const base = useId();
  const clear = (key: string) => setProblems((all) => (key in all ? Object.fromEntries(Object.entries(all).filter(([k]) => k !== key)) : all));

  const submit = async () => {
    const found: Record<string, string> = {};
    if (!NAME_PATTERN.test(name)) found.name = "Use lowercase letters, digits, '_' or '-' for the name.";
    for (const field of entry.fields) {
      if (field.required && !values[field.id]?.trim()) found[field.id] = `${field.label} is required.`;
    }
    setProblems(found);
    const first = ["name", ...entry.fields.map((field) => field.id)].find((key) => key in found);
    if (first) return focusField(`${base}-${first}`);
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
      <Field label="Name" required hint="Agents see its tools as mcp__name__tool." error={problems.name}>
        <input
          id={`${base}-name`}
          value={name}
          onChange={(event) => {
            setName(event.target.value.toLowerCase());
            clear("name");
          }}
          spellCheck={false}
        />
      </Field>
      {entry.fields.map((field) => (
        <CatalogInput key={field.id} id={`${base}-${field.id}`} field={field} value={values[field.id] ?? ""} error={problems[field.id]} onChange={(value) => { setValues((all) => ({ ...all, [field.id]: value })); clear(field.id); }} />
      ))}
      <p className="muted connector-runs">
        <code>{runs}</code>
      </p>
    </FormShell>
  );
}

function CatalogInput({ id, field, value, error, onChange }: { id: string; field: CatalogField; value: string; error?: string; onChange: (value: string) => void }) {
  const hint =
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
    <Field label={field.label} required={field.required} hint={hint} error={error}>
      {field.kind === "path_list" ? (
        <textarea id={id} rows={3} value={value} placeholder={field.placeholder ?? undefined} onChange={(event) => onChange(event.target.value)} spellCheck={false} />
      ) : (
        <input
          id={id}
         
          type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
          autoComplete={field.kind === "secret" ? "new-password" : "off"}
          value={value}
          placeholder={field.placeholder ?? undefined}
          onChange={(event) => onChange(event.target.value)}
          spellCheck={false}
        />
      )}
    </Field>
  );
}
