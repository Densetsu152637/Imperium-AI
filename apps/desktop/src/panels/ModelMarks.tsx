// The favourite and hide toggles on a model row, and the collapsed "Hidden" group at the end of a
// model list. The marks themselves are kept in the settings (modelPrefs.ts).

import { ChevronDown, ChevronRight, Eye, EyeOff, Star } from "lucide-react";

import { IconButton } from "../components/IconButton";
import type { ModelMark } from "../models/modelPrefs";

/** A star (favourite) and an eye (hide / show again) for one model. */
export function ModelMarkButtons({
  name,
  mark,
  onToggle,
}: {
  name: string;
  mark: ModelMark | null;
  onToggle: (mark: ModelMark) => void;
}) {
  const favourite = mark === "favourite";
  const hidden = mark === "hidden";
  return (
    <span className="model-marks">
      <IconButton
        size="sm"
        icon={Star}
        className={`model-mark favourite${favourite ? " on" : ""}`}
        aria-pressed={favourite}
        label={`Favourite ${name}`}
        tooltip={favourite ? "Favourite — listed first; click to unfavourite" : "Favourite: list it first when choosing a model"}
        onClick={() => onToggle("favourite")}
      />
      <IconButton
        size="sm"
        icon={hidden ? Eye : EyeOff}
        className={`model-mark${hidden ? " on" : ""}`}
        aria-pressed={hidden}
        label={`Hide ${name}`}
        tooltip={hidden ? "Hidden — click to show it again" : "Hide: leave it out of the session model lists"}
        onClick={() => onToggle("hidden")}
      />
    </span>
  );
}

/** The row that opens and closes the hidden models at the end of a table. */
export function HiddenGroupRow({ count, open, colSpan, onToggle }: { count: number; open: boolean; colSpan: number; onToggle: () => void }) {
  return (
    <tr role="row" className="model-hidden-group">
      <td role="cell" colSpan={colSpan}>
        <button type="button" className="ghost small" aria-expanded={open} onClick={onToggle}>
          {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />} Hidden ({count})
        </button>
      </td>
    </tr>
  );
}
