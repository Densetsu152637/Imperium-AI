// The live preview (roadmap item 9): the page the open session's project serves, shown beside
// the chat. An agent opens it with `preview_open` (a dev server it starts, or a localhost
// address); the user can also type an address. Only loopback pages are shown (preview.ts); the
// dev server reloads the page itself when files change, and Reload fetches it again.

import { RotateCw } from "lucide-react";
import { type FormEvent, useState } from "react";

import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import { previewUrl } from "../sessions/preview";
import type { SessionState } from "../sessions/sessions";

/** An address the user typed, and the agent's page it replaced (a newer agent page wins). */
type Typed = { url: string; over: string | null };

export function PreviewPanel({ session }: { session: SessionState | null }) {
  const sessionId = session?.summary.session_id ?? null;
  const [typed, setTyped] = useState<Record<string, Typed>>({});
  const [draft, setDraft] = useState<{ for: string | null; text: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloads, setReloads] = useState(0);

  if (!session || !sessionId) {
    return <EmptyState className="compact" title="No session open" body="Open a session to preview what it builds." />;
  }

  const agent = session.preview;
  const own = typed[sessionId];
  const candidate = own && own.over === (agent?.url ?? null) ? own.url : (agent?.url ?? own?.url ?? null);
  // Whatever the source (an agent's event, a typed address), only a page that passes the rule is framed.
  const checkedShown = candidate ? previewUrl(candidate) : null;
  const shown = checkedShown?.ok ? checkedShown.url : null;
  const text = draft && draft.for === sessionId ? draft.text : (shown ?? "");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const checked = previewUrl(text);
    if (!checked.ok) {
      setError(checked.reason);
      return;
    }
    setError(null);
    setDraft(null);
    setTyped({ ...typed, [sessionId]: { url: checked.url, over: agent?.url ?? null } });
  };

  return (
    <div className="preview">
      <form className="preview-bar" onSubmit={submit}>
        <input
          type="text"
          value={text}
          placeholder="http://localhost:5173"
          aria-label="Preview address"
          spellCheck={false}
          onChange={(event) => setDraft({ for: sessionId, text: event.target.value })}
        />
        <button type="submit" className="ghost">
          Show
        </button>
        <IconButton
          icon={RotateCw}
          label="Reload the preview"
          tooltip="Reload"
          disabled={!shown}
          onClick={() => setReloads((count) => count + 1)}
        />
      </form>
      {checkedShown && !checkedShown.ok && <Notice tone="danger">{checkedShown.reason}</Notice>}
      {error && <Notice tone="danger">{error}</Notice>}
      {agent?.command && (
        <Notice tone="info">
          Started with <code>{agent.command}</code>; it stops when the session ends.
        </Notice>
      )}
      {shown ? (
        <iframe
          key={`${shown}#${reloads}`}
          className="preview-frame"
          src={shown}
          title="Live preview"
          sandbox="allow-scripts allow-same-origin allow-forms allow-modals"
          referrerPolicy="no-referrer"
        />
      ) : (
        <EmptyState
          className="compact"
          title="No preview yet"
          body={
            <>
              The preview appears here when an agent opens one with <code>preview_open</code>. You can also type a
              localhost address above.
            </>
          }
        />
      )}
    </div>
  );
}
