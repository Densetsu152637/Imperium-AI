// A labelled form control: the label above the control, then an optional hint and error, all on one
// left edge. It wraps one input, textarea or Select and wires the label (`htmlFor`), the hint and
// the error (`aria-describedby`) and the invalid state to it, and gives a native input or textarea
// the shared `field` look. Layout classes only in `className`.

import { cloneElement, isValidElement, useId, type ReactElement, type ReactNode } from "react";

type ControlProps = { id?: string; className?: string; "aria-describedby"?: string; "aria-invalid"?: boolean | "true" | "false" };

export function Field({
  label,
  hint,
  error,
  required,
  className,
  children,
}: {
  label: ReactNode;
  /** Help under the control. */
  hint?: ReactNode;
  /** What is wrong, under the control, announced when it appears; marks the control invalid. */
  error?: ReactNode;
  required?: boolean;
  className?: string;
  children: ReactElement<ControlProps>;
}) {
  const base = useId();
  const hintId = hint ? `${base}-hint` : undefined;
  const errorId = error ? `${base}-error` : undefined;
  const control = isValidElement<ControlProps>(children) ? children : null;
  const controlId = control?.props.id ?? `${base}-control`;
  const native = control?.type === "input" || control?.type === "textarea";
  const describedBy = [control?.props["aria-describedby"], hintId, errorId].filter(Boolean).join(" ") || undefined;

  return (
    <div className={`field-stack${className ? ` ${className}` : ""}`}>
      <label className="field-label" htmlFor={controlId}>
        {label}
        {required && (
          <span className="field-required" aria-hidden="true">
            {" "}
            *
          </span>
        )}
      </label>
      {control &&
        cloneElement(control, {
          id: controlId,
          className: native ? ["field", control.props.className].filter(Boolean).join(" ") : control.props.className,
          "aria-describedby": describedBy,
          "aria-invalid": error ? true : control.props["aria-invalid"],
        })}
      {hint && (
        <p id={hintId} className="field-hint">
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} className="field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
