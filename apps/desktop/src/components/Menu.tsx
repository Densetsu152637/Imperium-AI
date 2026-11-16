// A menu: a button that opens a small list of actions, each with an icon, a label, an optional
// shortcut and, between groups, a separator; a danger item is drawn in the danger colour. Rows,
// panel headers and windows use it as their one overflow menu (a three-dots button), and it also
// serves a menu with a visible label. It follows the WAI-ARIA menu button pattern: arrows and
// Home/End move (key handling in shared/keyNav.ts), Enter or Space runs an item, Escape or a click
// outside closes. Like Select, the menu is rendered into the body so panels that clip their content
// do not cut it off, and both place themselves with shared/floating.ts.

import { Ellipsis, type LucideIcon } from "lucide-react";
import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";

import { edge, menuKey, stepWrap } from "../shared/keyNav";
import { placeList } from "../shared/floating";
import { Kbd } from "./Kbd";
import { Tooltip } from "./Tooltip";

export type MenuItem = {
  label: string;
  icon?: LucideIcon;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** A longer explanation, shown as the item's tooltip. */
  title?: string;
  /** The item's key chord, shown at the right edge (a reminder: the menu does not bind it). */
  shortcut?: string;
  /** Draw a separator above this item, to start a new group. */
  separatorBefore?: boolean;
};

/** Room an item takes, for deciding whether the menu fits under its button. */
const ITEM_ROOM = 34;

export function Menu({
  label,
  items,
  className,
  icon: TriggerIcon = Ellipsis,
  text,
}: {
  /** The button's accessible name and tooltip, and the menu's name. */
  label: string;
  items: MenuItem[];
  /** Extra classes for the button (e.g. `panel-menu-button`). */
  className?: string;
  /** The button's icon: three dots unless the menu is better named by an icon of its own. */
  icon?: LucideIcon;
  /** Words beside the icon, for a menu that reads better with a visible label. */
  text?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [position, setPosition] = useState<{ right: number; top: number; maxHeight: number; above: boolean } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLUListElement>(null);

  const place = useCallback(() => {
    const box = button.current?.getBoundingClientRect();
    if (!box) return;
    const placed = placeList(box, { width: window.innerWidth, height: window.innerHeight }, { needed: 16 + items.length * ITEM_ROOM, minHeight: 120 });
    setPosition({ right: placed.right, top: placed.top, maxHeight: placed.maxHeight, above: placed.above });
  }, [items.length]);

  const enabled = (index: number) => !items[index]?.disabled;

  const show = (index = edge(items.length, false, enabled)) => {
    place();
    setActive(Math.max(index, 0));
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

  // The active item stays in view in a menu that scrolls.
  useEffect(() => {
    if (open) menu.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

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
    const result = menuKey(event.key, active, items.length, enabled);
    if (!result) return;
    switch (result.kind) {
      case "move":
        if (result.active >= 0) setActive(result.active);
        break;
      case "run":
        run(result.active);
        break;
      case "close":
        // Only the menu closes, not the dialog around it.
        event.stopPropagation();
        close();
        break;
      case "leave":
        close(false);
        return;
    }
    event.preventDefault();
  };

  const trigger = (
    <button
      ref={button}
      type="button"
      className={`ghost${text ? "" : " icon-only"} row-menu-button${className ? ` ${className}` : ""}${open ? " open" : ""}`}
      aria-label={label}
      aria-haspopup="menu"
      aria-expanded={open}
      onClick={() => (open ? close() : show())}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          show(event.key === "ArrowDown" ? stepWrap(items.length, -1, 1, enabled) : stepWrap(items.length, items.length, -1, enabled));
        }
      }}
    >
      <TriggerIcon aria-hidden="true" />
      {text}
    </button>
  );

  return (
    <>
      {text ? trigger : <Tooltip text={label} describe={false}>{trigger}</Tooltip>}
      {open &&
        position &&
        createPortal(
          <ul
            ref={menu}
            role="menu"
            tabIndex={-1}
            aria-label={label}
            className={`row-menu${position.above ? " above" : ""}`}
            style={{ right: position.right, top: position.top, maxHeight: position.maxHeight }}
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
                <Fragment key={item.label}>
                  {item.separatorBefore && index > 0 && <li role="separator" className="row-menu-separator" />}
                  <li
                    role="menuitem"
                    data-index={index}
                    aria-disabled={item.disabled || undefined}
                    title={item.title}
                    className={`row-menu-item${index === active ? " active" : ""}${item.danger ? " danger" : ""}`}
                    onPointerMove={() => enabled(index) && index !== active && setActive(index)}
                    onClick={() => run(index)}
                  >
                    {Icon ? <Icon aria-hidden="true" /> : <span className="row-menu-spacer" />}
                    <span className="row-menu-label">{item.label}</span>
                    {item.shortcut && <Kbd className="row-menu-shortcut">{item.shortcut}</Kbd>}
                  </li>
                </Fragment>
              );
            })}
          </ul>,
          document.body,
        )}
    </>
  );
}
