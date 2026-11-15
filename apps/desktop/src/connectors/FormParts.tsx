// The frame and the fields the connector forms share.

import { ArrowLeft, LoaderCircle } from "lucide-react";
import type { FormEvent, ReactNode } from "react";
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
  error: string | null;
  children: ReactNode;
}) {
  return (
    <form
      className="connector-form"
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
          {subtitle && <div className="muted">{subtitle}</div>}
        </div>
      </div>
      {children}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="connector-form-actions">
        <button type="button" onClick={onBack}>
          Cancel
        </button>
        <button type="submit" className="primary" disabled={busy}>
          {busy && <LoaderCircle className="spin" aria-hidden="true" />} {submitLabel}
        </button>
      </div>
    </form>
  );
}

export function FormField({
  label,
  description,
  required,
  children,
  htmlFor,
}: {
  label: string;
  description?: ReactNode;
  required?: boolean;
  children: ReactNode;
  htmlFor: string;
}) {
  return (
    <div className="form-field">
      <label htmlFor={htmlFor}>
        {label}
        {required && <span className="required" aria-hidden="true"> *</span>}
      </label>
      {children}
      {description && <span className="form-field-description">{description}</span>}
    </div>
  );
}
