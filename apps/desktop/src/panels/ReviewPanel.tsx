// The review pane (roadmap item 6): the files the open session changed since one of its
// checkpoints (the session's start by default), each as a unified diff whose hunks are accepted
// (kept, and no longer shown) or rejected (reverted in the working tree). Under the files, a
// commit message prefilled from the session's title, *Commit*, and *Open pull request*; both ask
// before running unless a permission rule allows them. The checkpoint picker's menu also rewinds
// the session to a checkpoint (files, conversation, or both).

import { Check, GitCommitHorizontal, GitPullRequest, History, RefreshCw, Undo2 } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useState } from "react";
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

import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { LoadingState } from "../components/LoadingState";
import { Menu } from "../components/Menu";
import { Notice, type NoticeTone } from "../components/Notice";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";
import { checkpointLabel, filesIn, lineCounts, lineKind, repoName } from "../review/review";
import type { SessionState } from "../sessions/sessions";

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
  const [notice, setNotice] = useState<{ tone: NoticeTone; text: string } | null>(null);
  const sinceId = useId();
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
      setNotice({
        tone: "ok",
        text: `Committed ${result.files.length} file${result.files.length === 1 ? "" : "s"} as ${result.commit?.slice(0, 10) ?? "a new commit"}.`,
      });
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
        setNotice({ tone: "ok", text: result.url ? `Opened ${result.url}` : `Opened a pull request from ${result.branch}.` });
      } else {
        setCommand(result.command);
        setNotice({ tone: "warn", text: "GitHub's gh is not installed: run these commands to open the pull request." });
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
      setNotice({ tone: "info", text: parts.join(" ") });
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

  if (!session || !client) {
    return <EmptyState className="compact" title="No session open" body="Open a session to review what it changed." />;
  }
  if (review && !review.available) {
    return (
      <EmptyState
        className="compact"
        title="No review"
        body={review.reason ? `${review.reason.replace(/\.$/, "")}.` : "This session has no checkpoints."}
      />
    );
  }

  return (
    <div className="review">
      <div className="review-bar">
        <div className="review-since">
          <label htmlFor={sinceId}>Since</label>
          <Select
            id={sinceId}
            value={review?.since != null ? String(review.since) : ""}
            options={checkpoints.map((checkpoint) => ({
              value: String(checkpoint.number),
              label: checkpointLabel(checkpoint, checkpoint.number === firstTurn),
            }))}
            onChange={(value) => setSince(value ? Number(value) : null)}
            disabled={checkpoints.length === 0}
            placeholder="the session's start"
            aria-label="Review changes since this checkpoint"
          />
        </div>
        <span className="review-bar-tools">
          <IconButton icon={RefreshCw} label="Refresh the review" tooltip="Refresh" onClick={() => void load()} />
          {rewindItems.length > 0 && !busy && <Menu label="Rewind to a checkpoint" icon={History} items={rewindItems} />}
        </span>
      </div>
      {error && <Notice tone="danger">{error}</Notice>}
      {notice && <Notice tone={notice.tone}>{notice.text}</Notice>}
      {!review && !error && !busy && <LoadingState inline label="Loading changes…" />}
      {review && files.length === 0 && (
        <EmptyState
          className="compact"
          title="No changes to review"
          body={
            checkpoints.length === 0
              ? "A checkpoint is recorded before each turn that changes files."
              : "No changes left to review since this checkpoint."
          }
        />
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
            <Select
              value={chosenRepo ?? ""}
              options={repos.map((root) => ({ value: root, label: repoName(root) }))}
              onChange={setRepo}
              aria-label="Repository to commit in"
            />
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
      {command && (
        <pre className="review-command" aria-label="Commands to open the pull request">
          {command}
        </pre>
      )}
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
          <HunkTools disabled={disabled} what="this file" onAct={(action) => onAct(`file:${file.id}`, action)} />
        </summary>
        {file.binary ? (
          <EmptyState className="compact" title="Binary or large file" body="Accept or reject it whole." />
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
      <IconButton
        icon={Check}
        size="sm"
        className="row-tool"
        disabled={disabled}
        onClick={(event) => {
          event.preventDefault();
          onAct("accept");
        }}
        label={`Accept ${what}`}
        tooltip={`Accept ${what}: keep it and stop showing it`}
      />
      <IconButton
        icon={Undo2}
        size="sm"
        variant="danger"
        className="row-tool"
        disabled={disabled}
        onClick={(event) => {
          event.preventDefault();
          onAct("reject");
        }}
        label={`Reject ${what}`}
        tooltip={`Reject ${what}: revert it in the working tree`}
      />
    </span>
  );
}
