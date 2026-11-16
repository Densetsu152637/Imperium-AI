// Tabs following the WAI-ARIA tabs pattern: one tab is in the Tab order (roving tabindex), Left and
// Right move focus and select (Up and Down when `vertical`), Home and End jump to the first and last,
// disabled tabs are skipped. The panels stay the caller's: give each `tabPanelProps(idBase, id)` so
// the tab and its panel point at each other.

import type { LucideIcon } from "lucide-react";
import { useRef, type HTMLAttributes, type KeyboardEvent, type ReactNode } from "react";

import { tabKey } from "../shared/keyNav";

export type TabItem = {
  id: string;
  label: ReactNode;
  icon?: LucideIcon;
  disabled?: boolean;
  /** A count or short mark after the label. */
  badge?: ReactNode;
};

const tabDomId = (idBase: string, id: string) => `${idBase}-tab-${id}`;
const panelDomId = (idBase: string, id: string) => `${idBase}-panel-${id}`;

/** The attributes for the panel of tab `id`. */
export function tabPanelProps(idBase: string, id: string): HTMLAttributes<HTMLElement> {
  return { role: "tabpanel", id: panelDomId(idBase, id), "aria-labelledby": tabDomId(idBase, id), tabIndex: 0 };
}

export function Tabs({
  items,
  value,
  onChange,
  label,
  idBase,
  vertical = false,
  className,
}: {
  items: TabItem[];
  /** The selected tab's id. */
  value: string;
  onChange: (id: string) => void;
  /** The tab list's accessible name. */
  label: string;
  /** Prefix for the tab and panel ids; pass the same to `tabPanelProps`. */
  idBase: string;
  vertical?: boolean;
  className?: string;
}) {
  const list = useRef<HTMLDivElement>(null);
  const selected = items.findIndex((item) => item.id === value);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const from = items.findIndex((item) => tabDomId(idBase, item.id) === (event.target as HTMLElement).id);
    if (from < 0) return;
    const next = tabKey(event.key, from, items.length, (index) => !items[index].disabled, vertical);
    if (next === null || next < 0) return;
    event.preventDefault();
    onChange(items[next].id);
    list.current?.querySelector<HTMLElement>(`[id="${tabDomId(idBase, items[next].id)}"]`)?.focus();
  };

  return (
    <div
      ref={list}
      role="tablist"
      aria-label={label}
      aria-orientation={vertical ? "vertical" : "horizontal"}
      className={`tabs-list${vertical ? " vertical" : ""}${className ? ` ${className}` : ""}`}
      onKeyDown={onKeyDown}
    >
      {items.map((item, index) => {
        const Icon = item.icon;
        const active = index === selected;
        return (
          <button
            key={item.id}
            type="button"
            role="tab"
            id={tabDomId(idBase, item.id)}
            aria-selected={active}
            aria-controls={panelDomId(idBase, item.id)}
            // With no valid selection the first enabled tab stays reachable.
            tabIndex={active || (selected < 0 && index === items.findIndex((i) => !i.disabled)) ? 0 : -1}
            disabled={item.disabled}
            className={`tabs-tab${active ? " active" : ""}`}
            onClick={() => onChange(item.id)}
          >
            {Icon && <Icon aria-hidden="true" />}
            <span>{item.label}</span>
            {item.badge !== undefined && <span className="tabs-badge">{item.badge}</span>}
          </button>
        );
      })}
    </div>
  );
}
