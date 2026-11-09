// A select that matches the metallic theme: a button showing the current choice and a listbox
// popover with a check by the selected option. It follows the WAI-ARIA select-only combobox
// pattern: arrows, Home/End and Page keys move, typing jumps to a matching option, Enter or
// Space chooses, Escape or a click outside closes. The popover is rendered into the body so
// panels and dialogs that clip their content do not cut it off.

import { Check, ChevronDown } from "lucide-react";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";

export type SelectOption = { value: string; label: string; description?: string; disabled?: boolean };

type Props = {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** The accessible name, when no visible label points at the control with `id`. */
  "aria-label"?: string;
  id?: string;
  disabled?: boolean;
  /** Shown when `value` matches no option. */
  placeholder?: string;
  className?: string;
};

/** How long typed characters keep adding to the same search. */
const TYPEAHEAD_MS = 600;

export function Select({ value, options, onChange, disabled, placeholder = "Choose…", className, id, ...rest }: Props) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState<{ left: number; top: number; width: number; maxHeight: number; above: boolean } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const typed = useRef({ text: "", at: 0 });
  const listId = useId();
  const selectedIndex = options.findIndex((option) => option.value === value);
  const selected = selectedIndex >= 0 ? options[selectedIndex] : null;

  const enabled = useCallback((index: number) => !options[index]?.disabled, [options]);

  /** The next enabled option from `from` in direction `step`, staying in bounds. */
  const move = useCallback(
    (from: number, step: number) => {
      let index = from;
      for (let tries = 0; tries < options.length; tries++) {
        const next = Math.min(Math.max(index + step, 0), options.length - 1);
        if (next === index) break;
        index = next;
        if (enabled(index)) return index;
      }
      return enabled(from) ? from : index;
    },
    [options.length, enabled],
  );

  const place = useCallback(() => {
    const box = button.current?.getBoundingClientRect();
    if (!box) return;
    const below = window.innerHeight - box.bottom - 8;
    const aboveRoom = box.top - 8;
    const above = below < 180 && aboveRoom > below;
    setPosition({
      left: box.left,
      top: above ? box.top - 4 : box.bottom + 4,
      width: box.width,
      maxHeight: Math.max(Math.min(above ? aboveRoom : below, 320), 120),
      above,
    });
  }, []);

  const show = (index = selectedIndex >= 0 ? selectedIndex : move(-1, 1)) => {
    if (disabled || options.length === 0) return;
    place();
    setActive(index);
    setOpen(true);
  };

  const close = (refocus = true) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  const choose = (index: number) => {
    const option = options[index];
    if (!option || option.disabled) return;
    if (option.value !== value) onChange(option.value);
    close();
  };

  // Keep the popover attached to the button while anything scrolls or the window resizes.
  useLayoutEffect(() => {
    if (!open) return;
    list.current?.focus();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open, place]);

  // A press anywhere else closes it without choosing.
  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!list.current?.contains(target) && !button.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer, true);
    return () => document.removeEventListener("pointerdown", onPointer, true);
  }, [open]);

  // The active option stays in view.
  useEffect(() => {
    if (open) list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  /** Type-ahead: the next enabled option whose label starts with what was typed. */
  const search = (key: string, from: number) => {
    const now = Date.now();
    const text = now - typed.current.at < TYPEAHEAD_MS ? typed.current.text + key.toLowerCase() : key.toLowerCase();
    typed.current = { text, at: now };
    // Repeating one letter cycles through the options starting with it.
    const cycling = text.length > 1 && [...text].every((c) => c === text[0]);
    const needle = cycling ? text[0] : text;
    const start = cycling || text.length === 1 ? from + 1 : from;
    for (let offset = 0; offset < options.length; offset++) {
      const index = (start + offset) % options.length;
      if (enabled(index) && options[index].label.toLowerCase().startsWith(needle)) return index;
    }
    return -1;
  };

  const onButtonKey = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (disabled) return;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
      case "Enter":
      case " ":
        event.preventDefault();
        show();
        return;
      case "Home":
        event.preventDefault();
        show(move(-1, 1));
        return;
      case "End":
        event.preventDefault();
        show(move(options.length, -1));
        return;
      default:
        if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          // Like a native select, typing on the closed control changes the choice directly.
          const found = search(event.key, selectedIndex);
          if (found >= 0 && options[found].value !== value) onChange(options[found].value);
        }
    }
  };

  const onListKey = (event: KeyboardEvent<HTMLUListElement>) => {
    const page = 8;
    switch (event.key) {
      case "ArrowDown":
        setActive((index) => move(index, 1));
        break;
      case "ArrowUp":
        if (event.altKey) {
          choose(active);
          break;
        }
        setActive((index) => move(index, -1));
        break;
      case "PageDown":
        setActive((index) => move(Math.min(index + page - 1, options.length - 1), 1));
        break;
      case "PageUp":
        setActive((index) => move(Math.max(index - page + 1, 0), -1));
        break;
      case "Home":
        setActive(move(-1, 1));
        break;
      case "End":
        setActive(move(options.length, -1));
        break;
      case "Enter":
      case " ":
        choose(active);
        break;
      case "Escape":
        // Only the popover closes, not the dialog around it.
        event.stopPropagation();
        close();
        break;
      case "Tab":
        close(false);
        return;
      default: {
        if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          const found = search(event.key, active);
          if (found >= 0) setActive(found);
          break;
        }
        return;
      }
    }
    event.preventDefault();
  };

  const optionId = (index: number) => `${listId}-option-${index}`;

  return (
    <>
      <button
        ref={button}
        id={id}
        type="button"
        role="combobox"
        className={`select${open ? " open" : ""}${className ? ` ${className}` : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-label={rest["aria-label"]}
        disabled={disabled}
        onClick={() => (open ? close() : show())}
        onKeyDown={onButtonKey}
      >
        <span className={`select-value${selected ? "" : " placeholder"}`}>{selected?.label ?? (value || placeholder)}</span>
        <ChevronDown className="select-chevron" aria-hidden="true" />
      </button>
      {open &&
        position &&
        createPortal(
          <ul
            ref={list}
            id={listId}
            role="listbox"
            tabIndex={-1}
            className={`select-list${position.above ? " above" : ""}`}
            aria-label={rest["aria-label"]}
            aria-labelledby={rest["aria-label"] ? undefined : id}
            aria-activedescendant={optionId(active)}
            style={{
              left: position.left,
              top: position.top,
              minWidth: position.width,
              maxHeight: position.maxHeight,
            }}
            onKeyDown={onListKey}
            onBlur={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node | null) && event.relatedTarget !== button.current) {
                setOpen(false);
              }
            }}
          >
            {options.map((option, index) => (
              <li
                key={option.value}
                id={optionId(index)}
                data-index={index}
                role="option"
                aria-selected={option.value === value}
                aria-disabled={option.disabled || undefined}
                className={`select-option${index === active ? " active" : ""}`}
                onPointerMove={() => !option.disabled && index !== active && setActive(index)}
                onClick={() => choose(index)}
              >
                <span className="select-check" aria-hidden="true">
                  {option.value === value && <Check />}
                </span>
                <span className="select-option-text">
                  <span>{option.label}</span>
                  {option.description && <span className="select-option-description">{option.description}</span>}
                </span>
              </li>
            ))}
          </ul>,
          document.body,
        )}
    </>
  );
}
