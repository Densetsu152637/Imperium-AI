// A "more" menu: a three-dots button that opens a small menu of actions. Rows, panel headers and
// windows use it as their one overflow menu, so only the primary action stays on screen. It follows
// the WAI-ARIA menu button pattern: arrows and Home/End move, Enter or Space runs an item, Escape or a
// click outside closes. Like Select, the menu is rendered into the body so panels that clip their
// content do not cut it off.

import { Ellipsis, type LucideIcon } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";

export type RowMenuItem = {
  label: string;
  icon?: LucideIcon;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** A longer explanation, shown as the item's tooltip. */
  title?: string;
};

export function RowMenu({
  label,
  items,
  className,
  icon: TriggerIcon = Ellipsis,
  text,
}: {
  /** The button's accessible name and tooltip, and the menu's name. */
  label: string;
  items: RowMenuItem[];
  /** Extra classes for the button (e.g. `panel-menu-button`). */
  className?: string;
  /** The button's icon: three dots unless the menu is better named by an icon of its own. */
  icon?: LucideIcon;
  /** Words beside the icon, for a menu that reads better with a visible label. */
  text?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState<{ right: number; top: number; above: boolean } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLUListElement>(null);

  const place = useCallback(() => {
    const box = button.current?.getBoundingClientRect();
    if (!box) return;
    const above = window.innerHeight - box.bottom < 40 + items.length * 34 && box.top > window.innerHeight / 2;
    setPosition({ right: window.innerWidth - box.right, top: above ? box.top - 4 : box.bottom + 4, above });
  }, [items.length]);

  const enabled = (index: number) => !items[index]?.disabled;
  const step = (from: number, by: number) => {
    for (let i = 1; i <= items.length; i++) {
      const next = (from + by * i + items.length * 2) % items.length;
      if (enabled(next)) return next;
    }
    return from;
  };

  const show = (index = step(-1, 1)) => {
    place();
    setActive(index);
    setOpen(true);
  };
  const close = (refocus = true) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };
  const run = (index: number) => {
    const item = items[index];
    if (!item || item.disabled) return;
    close();
    item.onSelect();
  };

  useLayoutEffect(() => {
    if (!open) return;
    menu.current?.focus();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!menu.current?.contains(target) && !button.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer, true);
    return () => document.removeEventListener("pointerdown", onPointer, true);
  }, [open]);

  const onMenuKey = (event: KeyboardEvent<HTMLUListElement>) => {
    switch (event.key) {
      case "ArrowDown":
        setActive((index) => step(index, 1));
        break;
      case "ArrowUp":
        setActive((index) => step(index, -1));
        break;
      case "Home":
        setActive(step(-1, 1));
        break;
      case "End":
        setActive(step(items.length, -1));
        break;
      case "Enter":
      case " ":
        run(active);
        break;
      case "Escape":
        event.stopPropagation();
        close();
        break;
      case "Tab":
        close(false);
        return;
      default:
        return;
    }
    event.preventDefault();
  };

  return (
    <>
      <button
        ref={button}
        type="button"
        className={`ghost${text ? "" : " icon-only"} row-menu-button${className ? ` ${className}` : ""}${open ? " open" : ""}`}
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => (open ? close() : show())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            show(event.key === "ArrowDown" ? step(-1, 1) : step(items.length, -1));
          }
        }}
      >
        <TriggerIcon aria-hidden="true" />
        {text}
      </button>
      {open &&
        position &&
        createPortal(
          <ul
            ref={menu}
            role="menu"
            tabIndex={-1}
            aria-label={label}
            className={`row-menu${position.above ? " above" : ""}`}
            style={{ right: position.right, top: position.top }}
            onKeyDown={onMenuKey}
            onBlur={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node | null) && event.relatedTarget !== button.current) {
                setOpen(false);
              }
            }}
          >
            {items.map((item, index) => {
              const Icon = item.icon;
              return (
                <li
                  key={item.label}
                  role="menuitem"
                  aria-disabled={item.disabled || undefined}
                  title={item.title}
                  className={`row-menu-item${index === active ? " active" : ""}${item.danger ? " danger" : ""}`}
                  onPointerMove={() => enabled(index) && index !== active && setActive(index)}
                  onClick={() => run(index)}
                >
                  {Icon ? <Icon aria-hidden="true" /> : <span className="row-menu-spacer" />}
                  <span>{item.label}</span>
                </li>
              );
            })}
          </ul>,
          document.body,
        )}
    </>
  );
}
