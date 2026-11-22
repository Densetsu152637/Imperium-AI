// The picker `@` opens in the composer: the session's files and directories that match what was
// typed, best first. The composer's text field keeps focus and drives it (arrow keys, Enter, Esc),
// so this is a listbox the field points at with `aria-activedescendant`.

import { File, Folder } from "lucide-react";

import type { SessionFile } from "@imperium/protocol";

type Props = {
  id: string;
  files: SessionFile[];
  index: number;
  truncated: boolean;
  onChoose: (file: SessionFile) => void;
  onHover: (index: number) => void;
};

export const mentionOptionId = (id: string, index: number) => `${id}-option-${index}`;

export function MentionPicker({ id, files, index, truncated, onChoose, onHover }: Props) {
  return (
    <ul className="mention-picker" id={id} role="listbox" aria-label="Files to mention">
      {files.map((file, position) => {
        const slash = file.relative.lastIndexOf("/");
        const name = file.relative.slice(slash + 1);
        const folder = slash >= 0 ? file.relative.slice(0, slash) : "";
        return (
          <li
            key={file.path}
            id={mentionOptionId(id, position)}
            role="option"
            aria-selected={position === index}
            className={`mention-option${position === index ? " active" : ""}`}
            title={file.path}
            // The field keeps focus: choosing on mouse-down stops it blurring first.
            onMouseDown={(event) => {
              event.preventDefault();
              onChoose(file);
            }}
            onMouseMove={() => position !== index && onHover(position)}
          >
            {file.directory ? <Folder aria-hidden="true" /> : <File aria-hidden="true" />}
            <span className="mention-name">
              {name}
              {file.directory && "/"}
            </span>
            {folder && <span className="mention-folder muted">{folder}</span>}
          </li>
        );
      })}
      {truncated && <li className="mention-more muted" role="presentation">Keep typing to narrow the list</li>}
    </ul>
  );
}
