// The review pane (roadmap item 6): the files the open session changed since one of its
// checkpoints (the session's start by default), each as a unified diff whose hunks are accepted
// (kept, and no longer shown) or rejected (reverted in the working tree). Under the files, a
// commit message prefilled from the session's title, *Commit*, and *Open pull request*; both ask
// before running unless a permission rule allows them. The checkpoint picker's menu also rewinds
// the session to a checkpoint (files, conversation, or both).

import { Check, GitCommitHorizontal, GitPullRequest, History, RefreshCw, Undo2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import type {
  ReviewFile,
  ReviewHunk,
  RewindMode,
  SessionReviewCommitResult,
  SessionReviewHunkResult,
  SessionReviewPullRequestResult,
  SessionReviewResult,
  SessionRewindResult,
} from "@imperium/protocol";

import { RowMenu } from "../components/RowMenu";
import type { DaemonClient } from "../daemon/client";
import { checkpointLabel, filesIn, lineCounts, lineKind, repoName } from "../review";
import type { SessionState } from "../sessions";

function message(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}

/** Something waiting for a yes: a commit or a pull request the permission rules ask about. */
type Pending = { kind: "commit"; files: string[] } | { kind: "pr" };

export function ReviewPanel({
  session,
  client,
  onOpenSession,
}: {
  session: SessionState | null;
  client: DaemonClient | null;
  /** Opens a session (the fork a conversation rewind makes). */
  onOpenSession?: (sessionId: string) => void;
}) {
  const sessionId = session?.summary.session_id ?? null;
  const busy = session?.busy ?? false;
  const [since, setSince] = useState<number | null>(null);
  const [review, setReview] = useState<SessionReviewResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const [repo, setRepo] = useState<string | null>(null);
  const [commitMessage, setCommitMessage] = useState("");
  const [edited, setEdited] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  const [command, setCommand] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!client || !sessionId) return;
    try {
      const result = await client.call<SessionReviewResult>("session.review", {
        session_id: sessionId,
        since,
      });
      setReview(result);
      setError(null);
    } catch (failure) {
      setError(message(failure));
    }
  }, [client, sessionId, since]);

  // A new session starts from its own start.
  useEffect(() => {
    setSince(null);
    setReview(null);
    setNotice(null);
    setPending(null);
    setCommand(null);
    setEdited(false);
  }, [sessionId]);

  // Read again when the session settles after a turn, and when it records a checkpoint.
  useEffect(() => {
    if (!busy) void load();
  }, [load, busy]);
  useEffect(() => {
    if (!client || !sessionId) return;
    return client.onNotification((notification) => {
      const params = notification.params as { session_id?: string } | undefined;
      if (notification.method === "session.checkpoint" && params?.session_id === sessionId) void load();
    });
  }, [client, sessionId, load]);

  const repos = review?.repos ?? [];
  const chosenRepo = repo && repos.includes(repo) ? repo : (repos[0] ?? null);
  const files = review?.files ?? [];
  const repoFiles = chosenRepo ? filesIn(files, chosenRepo) : [];
  useEffect(() => {
    if (!edited) setCommitMessage(review?.suggested_message ?? "");
  }, [review?.suggested_message, edited]);

  const run = async (work: () => Promise<void>) => {
    setWorking(true);
    setError(null);
    try {
      await work();
    } catch (failure) {
      setError(message(failure));
    } finally {
      setWorking(false);
    }
  };

  const actOnHunk = (file: ReviewFile, hunk: string, action: "accept" | "reject") =>
    run(async () => {
      const result = await client!.call<SessionReviewHunkResult>("session.review_hunk", {
        session_id: sessionId,
        since: review?.since ?? null,
        repo: file.repo,
        path: file.path,
        hunk,
        action,
      });
      setReview((current) => {
        if (!current) return current;
        const others = current.files.filter((f) => !(f.repo === file.repo && f.path === file.path));
        const index = current.files.findIndex((f) => f.repo === file.repo && f.path === file.path);
        const next = result.file ? [...others.slice(0, index), result.file, ...others.slice(index)] : others;
        return { ...current, files: next };
      });
    });

  const commit = (confirmed: boolean) =>
    run(async () => {
      if (!chosenRepo) return;
      const result = await client!.call<SessionReviewCommitResult>("session.review_commit", {
        session_id: sessionId,
        since: review?.since ?? null,
        repo: chosenRepo,
        message: commitMessage,
        confirmed,
      });
      if (result.needs_confirmation) {
        setPending({ kind: "commit", files: result.files });
        return;
      }
      setPending(null);
      setEdited(false);
      setNotice(`Committed ${result.files.length} file${result.files.length === 1 ? "" : "s"} as ${result.commit?.slice(0, 10) ?? "a new commit"}.`);
      await load();
    });

  const openPullRequest = (confirmed: boolean) =>
    run(async () => {
      if (!chosenRepo) return;
      const [title, ...body] = commitMessage.split("\n");
      const result = await client!.call<SessionReviewPullRequestResult>("session.review_pull_request", {
        session_id: sessionId,
        repo: chosenRepo,
        title: title ?? "",
        body: body.join("\n").trim(),
        base: null,
        confirmed,
      });
      if (result.needs_confirmation) {
        setPending({ kind: "pr" });
        setCommand(result.command);
        return;
      }
      setPending(null);
      if (result.opened) {
        setCommand(null);
        setNotice(result.url ? `Opened ${result.url}` : `Opened a pull request from ${result.branch}.`);
      } else {
        setCommand(result.command);
        setNotice("GitHub's gh is not installed: run these commands to open the pull request.");
      }
    });

  const rewind = (checkpoint: number, mode: RewindMode) =>
    run(async () => {
      const result = await client!.call<SessionRewindResult>("session.rewind", {
        session_id: sessionId,
        checkpoint,
        mode,
      });
      const parts: string[] = [];
      if (result.undo_checkpoint != null) {
        parts.push(
          `Files rewound (${result.restored.length} restored, ${result.removed.length} removed); checkpoint #${result.undo_checkpoint} undoes it.`,
        );
      }
      if (result.session_id) {
        parts.push("The conversation continues in a new session.");
        onOpenSession?.(result.session_id);
      }
      setNotice(parts.join(" "));
      await load();
    });

  const checkpoints = review?.checkpoints ?? [];
  const firstTurn = checkpoints.find((c) => c.kind === "turn")?.number ?? null;
  const rewindItems = useMemo(
    () =>
      checkpoints
        .slice()
        .reverse()
        .flatMap((checkpoint) => {
          const label = checkpointLabel(checkpoint, checkpoint.number === firstTurn);
          const items = [
            {
              label: `Rewind files to ${label}`,
              icon: Undo2,
              onSelect: () => void rewind(checkpoint.number, "files"),
            },
          ];
          if (checkpoint.conversation) {
            items.push({
              label: `Rewind files and conversation to ${label}`,
              icon: History,
              onSelect: () => void rewind(checkpoint.number, "both"),
            });
            items.push({
              label: `Rewind only the conversation to ${label}`,
              icon: History,
              onSelect: () => void rewind(checkpoint.number, "conversation"),
            });
          }
          return items;
        }),
    // `rewind` is a fresh closure each render; the list follows the checkpoints.
    [checkpoints, firstTurn, sessionId, client],
  );

  if (!session || !client) return <p className="empty">Open a session to review what it changed.</p>;
  if (review && !review.available) {
    return <p className="empty">No review: {review.reason ?? "this session has no checkpoints"}.</p>;
  }

  return (
    <div className="review">
      <div className="review-bar">
        <label className="review-since">
          <span>Since</span>
          <select
            value={review?.since ?? ""}
            onChange={(event) => setSince(event.target.value ? Number(event.target.value) : null)}
            disabled={checkpoints.length === 0}
            aria-label="Review changes since this checkpoint"
          >
            {checkpoints.length === 0 && <option value="">the session's start</option>}
            {checkpoints.map((checkpoint) => (
              <option key={checkpoint.number} value={checkpoint.number}>
                {checkpointLabel(checkpoint, checkpoint.number === firstTurn)}
              </option>
            ))}
          </select>
        </label>
        <button className="ghost icon-only" onClick={() => void load()} aria-label="Refresh the review" title="Refresh">
          <RefreshCw aria-hidden="true" />
        </button>
        {rewindItems.length > 0 && !busy && (
          <RowMenu label="Rewind to a checkpoint" icon={History} items={rewindItems} />
        )}
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      {notice && <p className="review-notice" role="status">{notice}</p>}
      {review && files.length === 0 && (
        <p className="empty">
          {checkpoints.length === 0
            ? "Nothing changed yet: a checkpoint is recorded before each turn that changes files."
            : "No changes left to review since this checkpoint."}
        </p>
      )}
      <ul className="review-files" aria-label="Changed files">
        {files.map((file) => (
          <ReviewFileView
            key={`${file.repo}\u0000${file.path}`}
            file={file}
            showRepo={repos.length > 1}
            disabled={working || busy}
            onAct={(hunk, action) => void actOnHunk(file, hunk, action)}
          />
        ))}
      </ul>
      {repoFiles.length > 0 && (
        <div className="review-commit">
          {repos.length > 1 && (
            <select value={chosenRepo ?? ""} onChange={(event) => setRepo(event.target.value)} aria-label="Repository to commit in">
              {repos.map((root) => (
                <option key={root} value={root}>
                  {repoName(root)}
                </option>
              ))}
            </select>
          )}
          <textarea
            value={commitMessage}
            onChange={(event) => {
              setCommitMessage(event.target.value);
              setEdited(true);
            }}
            rows={3}
            aria-label="Commit message"
            placeholder="Commit message"
          />
          {pending ? (
            <div className="review-confirm" role="alertdialog" aria-label="Confirm">
              <span>
                {pending.kind === "commit"
                  ? `Commit ${pending.files.length} file${pending.files.length === 1 ? "" : "s"} to ${repoName(chosenRepo ?? "")}?`
                  : "Push the branch and open a pull request?"}
              </span>
              <button className="primary small" disabled={working} onClick={() => void (pending.kind === "commit" ? commit(true) : openPullRequest(true))}>
                {pending.kind === "commit" ? "Commit" : "Open"}
              </button>
              <button className="ghost small" onClick={() => setPending(null)}>
                Cancel
              </button>
            </div>
          ) : (
            <div className="review-actions">
              <button className="primary small" disabled={working || busy || !commitMessage.trim()} onClick={() => void commit(false)}>
                <GitCommitHorizontal aria-hidden="true" /> Commit
              </button>
              <button className="ghost small" disabled={working || busy || !commitMessage.trim()} onClick={() => void openPullRequest(false)}>
                <GitPullRequest aria-hidden="true" /> Open pull request
              </button>
            </div>
          )}
        </div>
      )}
      {command && <pre className="review-command" aria-label="Commands to open the pull request">{command}</pre>}
    </div>
  );
}

function ReviewFileView({
  file,
  showRepo,
  disabled,
  onAct,
}: {
  file: ReviewFile;
  showRepo: boolean;
  disabled: boolean;
  onAct: (hunk: string, action: "accept" | "reject") => void;
}) {
  const counts = lineCounts(file);
  return (
    <li className="review-file">
      <details open>
        <summary>
          <span className={`review-status review-${file.status}`} title={file.status}>
            {file.status === "added" ? "A" : file.status === "deleted" ? "D" : "M"}
          </span>
          <span className="review-path" title={file.path}>
            {showRepo && <span className="review-repo">{repoName(file.repo)}/</span>}
            {file.path}
          </span>
          {!file.binary && (
            <span className="review-counts">
              <span className="review-add">+{counts.added}</span> <span className="review-del">−{counts.removed}</span>
            </span>
          )}
          <HunkTools disabled={disabled} what="this file" onAct={(action) => onAct("file", action)} />
        </summary>
        {file.binary ? (
          <p className="empty">Binary or large file: accept or reject it whole.</p>
        ) : (
          file.hunks.map((hunk) => (
            <HunkView key={hunk.id} hunk={hunk} disabled={disabled} onAct={(action) => onAct(hunk.id, action)} />
          ))
        )}
      </details>
    </li>
  );
}

function HunkView({ hunk, disabled, onAct }: { hunk: ReviewHunk; disabled: boolean; onAct: (action: "accept" | "reject") => void }) {
  return (
    <div className="review-hunk">
      <div className="review-hunk-header">
        <code>{hunk.header}</code>
        <HunkTools disabled={disabled} what="this change" onAct={onAct} />
      </div>
      <pre className="review-lines">
        {hunk.lines.map((line, index) => (
          <span key={index} className={`review-line review-line-${lineKind(line)}`}>
            {line}
            {"\n"}
          </span>
        ))}
      </pre>
    </div>
  );
}

function HunkTools({ disabled, what, onAct }: { disabled: boolean; what: string; onAct: (action: "accept" | "reject") => void }) {
  return (
    <span className="review-tools">
      <button
        className="ghost icon-only row-tool"
        disabled={disabled}
        onClick={(event) => {
          event.preventDefault();
          onAct("accept");
        }}
        aria-label={`Accept ${what}`}
        title={`Accept ${what}: keep it and stop showing it`}
      >
        <Check aria-hidden="true" />
      </button>
      <button
        className="ghost icon-only row-tool danger"
        disabled={disabled}
        onClick={(event) => {
          event.preventDefault();
          onAct("reject");
        }}
        aria-label={`Reject ${what}`}
        title={`Reject ${what}: revert it in the working tree`}
      >
        <Undo2 aria-hidden="true" />
      </button>
    </span>
  );
}
