// The frame and the group field the connector forms share. Single controls use the shared `Field`.

import { ArrowLeft } from "lucide-react";
import type { FormEvent, ReactNode } from "react";
import { Notice } from "../components/Notice";
import { Spinner } from "../components/LoadingState";
import { ConnectorIcon } from "./connectorIcons";

export function FormShell({
  title,
  icon,
  subtitle,
  onBack,
  onSubmit,
  submitLabel,
  busy,
  error,
  children,
}: {
  title: string;
  icon?: string | null;
  subtitle?: ReactNode;
  onBack: () => void;
  onSubmit: () => void;
  submitLabel: string;
  busy: boolean;
  /** A failure that belongs to the whole form (the daemon refused the save); field problems show under their field. */
  error: string | null;
  children: ReactNode;
}) {
  return (
    <form
      className="connector-form"
      noValidate
      onSubmit={(event: FormEvent) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <button type="button" className="ghost back" onClick={onBack}>
        <ArrowLeft aria-hidden="true" /> Back
      </button>
      <div className="connector-form-head">
        <ConnectorIcon icon={icon} size="large" />
        <div>
          <h3>{title}</h3>
          {subtitle && <div className="connector-form-subtitle">{subtitle}</div>}
        </div>
      </div>
      {children}
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="connector-form-actions">
        <button type="button" onClick={onBack}>
          Cancel
        </button>
        <button type="submit" className="primary" disabled={busy}>
          {busy && <Spinner />} {submitLabel}
        </button>
      </div>
    </form>
  );
}

/** A labelled set of controls that `Field` cannot wrap (one label over several inputs), in the same stack as a `Field`. */
export function FieldGroup({
  label,
  hint,
  error,
  id,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: ReactNode;
  id: string;
  children: ReactNode;
}) {
  return (
    <div className="field-stack" role="group" aria-labelledby={`${id}-label`} aria-describedby={error ? `${id}-error` : hint ? `${id}-hint` : undefined}>
      <span id={`${id}-label`} className="field-label">
        {label}
      </span>
      {children}
      {hint && (
        <p id={`${id}-hint`} className="field-hint">
          {hint}
        </p>
      )}
      {error && (
        <p id={`${id}-error`} className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** Moves focus to the first control that has an error, once the errors are on screen. */
export function focusField(id: string) {
  window.requestAnimationFrame(() => document.getElementById(id)?.focus());
}
