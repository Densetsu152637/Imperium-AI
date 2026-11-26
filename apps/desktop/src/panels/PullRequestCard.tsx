// The pull-request card in the session view: the title and link, the state, the check counts with
// the failing names, the review and conflict state, and the auto-fix and auto-merge switches. The
// auto-fix switch is an ordinary daemon call (`pull_requests.set_monitor`), as is the
// acknowledgement of the notice that a session handled review text. The auto-merge switch
// turns on only through the app's own `auto_merge_enable` command, after a native confirmation
// that names the method and the branch (`sessions/autoMerge.ts`); turning it off is an ordinary
// daemon call. The daemon's cache (`pull_requests.status`) and its `pull_requests.changed`
// notification are the only source. Every string came from GitHub and is drawn as text
// (`sessions/prCard.ts` cleans it); the link opens in the user's own browser.

import { invoke, isTauri } from "@tauri-apps/api/core";
import { ChevronDown, ChevronRight, ExternalLink, GitMerge, GitPullRequest, GitPullRequestClosed, GitPullRequestDraft } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import type { ConfigDescribeResult, PrMergeMethod, PrStatus, PullRequestsChangedParams } from "@imperium/protocol";

import { Badge } from "../components/Badge";
import { Notice } from "../components/Notice";
import { Select } from "../components/Select";
import { Switch } from "../components/Switch";
import { ToolState } from "../components/ToolState";
import type { DaemonClient } from "../daemon/client";
import { message } from "../shared/plainText";
import { formatAgo } from "../shared/ago";
import { openExternal } from "../shared/openExternal";
import { problemOf, turnOffAutoMerge, turnOnAutoMerge } from "../sessions/autoMerge";
import {
  autoMergeSettings,
  DEFAULT_AUTO_MERGE_SETTINGS,
  describePr,
  isFirstRead,
  nextStatus,
  parseBindInput,
  refOfUrl,
  type AutoMergeSettings,
  type PrCardView,
  type StateIcon,
} from "../sessions/prCard";

const STATE_ICONS = {
  open: GitPullRequest,
  draft: GitPullRequestDraft,
  merged: GitMerge,
  closed: GitPullRequestClosed,
} as const satisfies Record<StateIcon, unknown>;

type Props = {
  sessionId: string;
  /** `SessionSummary.pull_request_url`: set while the session has a pull request bound. */
  url: string | null;
  archived: boolean;
  client: DaemonClient | null;
};

export function PullRequestCard({ sessionId, url, archived, client }: Props) {
  const [status, setStatus] = useState<PrStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settings, setSettings] = useState<AutoMergeSettings>(DEFAULT_AUTO_MERGE_SETTINGS);
  const bound = Boolean(url) && !archived;
  // Where focus goes when the card changes shape under the user's keys: to the bind link after
  // the binding ends, to the Details toggle after a bind.
  const focusAfter = useRef<"bind-link" | "details" | null>(null);

  // Follow the session's pull request: the daemon's cached status, then every change.
  useEffect(() => {
    setStatus(null);
    setLoadError(null);
    if (!client || !bound) return;
    let live = true;
    const off = client.onNotification((notification) => {
      if (notification.method !== "pull_requests.changed") return;
      const change = notification.params as PullRequestsChangedParams;
      if (change.session_id === sessionId) setStatus((shown) => nextStatus(shown, change.status, true));
    });
    client
      .call<PrStatus>("pull_requests.status", { session_id: sessionId })
      .then((answer) => live && setStatus((shown) => nextStatus(shown, answer, false)))
      .catch((error) => live && setLoadError(message(error)));
    return () => {
      live = false;
      off();
    };
  }, [client, sessionId, bound]);

  // The user's `github.automerge.*` settings: whether auto-merge may be turned on, and the method
  // to preselect. They change what the card offers, never whether anything is on.
  useEffect(() => {
    if (!client || !bound) return;
    let live = true;
    client
      .call<ConfigDescribeResult>("config.describe", {})
      .then((config) => live && setSettings(autoMergeSettings(config.entries)))
      .catch(() => live && setSettings(DEFAULT_AUTO_MERGE_SETTINGS));
    return () => {
      live = false;
    };
  }, [client, bound]);

  if (archived) {
    // The binding is gone with the archive; the link stays for the record.
    return url ? (
      <p className="pr-card pr-archived muted">
        <GitPullRequest aria-hidden="true" />
        <span>Pull request {refOfUrl(url)}</span>
      </p>
    ) : null;
  }
  if (!bound) {
    return client ? (
      <BindForm
        key={sessionId}
        sessionId={sessionId}
        client={client}
        focusLink={focusAfter.current === "bind-link"}
        onFocused={() => (focusAfter.current = null)}
        onBound={(bound) => {
          focusAfter.current = "details";
          setStatus(bound);
        }}
      />
    ) : null;
  }
  if (!status) {
    return (
      <section className="pr-card" aria-label="Pull request">
        {loadError && !isFirstRead(loadError) ? (
          <Notice tone="warn" title="Could not read the pull request">
            {loadError}
          </Notice>
        ) : (
          <p className="pr-loading muted" role="status">
            <GitPullRequest aria-hidden="true" /> Reading {refOfUrl(url!)}…{loadError ? ` ${loadError}` : ""}
          </p>
        )}
      </section>
    );
  }
  return (
    <Bound
      key={sessionId}
      status={status}
      settings={settings}
      sessionId={sessionId}
      client={client}
      onStatus={(changed) => setStatus((shown) => nextStatus(shown, changed, true))}
      focusToggle={focusAfter.current === "details"}
      onFocused={() => (focusAfter.current = null)}
      // The daemon announces the unbind (session.updated) before it answers, so the bind form can
      // mount first: ask for the focus before the call, and withdraw it if the call fails.
      onUnbinding={(started) => (focusAfter.current = started ? "bind-link" : null)}
      onUnbound={() => setStatus(null)}
    />
  );
}

function Bound({
  status,
  settings,
  sessionId,
  client,
  onStatus,
  focusToggle,
  onFocused,
  onUnbinding,
  onUnbound,
}: {
  status: PrStatus;
  settings: AutoMergeSettings;
  sessionId: string;
  client: DaemonClient | null;
  /** The daemon's answer to a change this card asked for. */
  onStatus: (status: PrStatus) => void;
  /** Focus the Details toggle once, as the card has just replaced the bind form. */
  focusToggle: boolean;
  onFocused: () => void;
  /** True just before `pull_requests.unbind` is sent, false if it failed. */
  onUnbinding: (started: boolean) => void;
  onUnbound: () => void;
}) {
  const toggle = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!focusToggle) return;
    toggle.current?.focus();
    onFocused();
  }, []);
  const view = describePr(status, settings);
  const [open, setOpen] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [chosen, setChosen] = useState<PrMergeMethod | null>(null);
  const [busy, setBusy] = useState(false);
  // The user's pick while it is still one the repository allows; otherwise the card's own choice.
  const method = chosen && view.methods.some((option) => option.value === chosen) ? chosen : view.method;
  const detailsId = useId();
  const Icon = STATE_ICONS[view.state.icon];
  const when = formatAgo(status.updated_at);

  const unbind = async () => {
    if (!client) return;
    setProblem(null);
    onUnbinding(true);
    try {
      await client.call("pull_requests.unbind", { session_id: sessionId });
      onUnbound();
    } catch (error) {
      onUnbinding(false);
      setProblem(message(error));
    }
  };

  /** The auto-fix switch and the acknowledgement of untrusted text are daemon calls. */
  const changeMonitor = async (change: { auto_fix?: boolean; ack_untrusted?: boolean }) => {
    if (!client || busy) return;
    setProblem(null);
    setBusy(true);
    try {
      onStatus(await client.call<PrStatus>("pull_requests.set_monitor", { session_id: sessionId, url: status.url, ...change }));
    } catch (error) {
      setProblem(message(error));
    } finally {
      setBusy(false);
    }
  };

  /** The auto-merge switch: off asks the daemon; on asks the app, which asks the user first. */
  const toggleAutoMerge = async () => {
    if (!client || busy) return;
    setProblem(null);
    setBusy(true);
    try {
      if (view.autoMerge.on) {
        onStatus(await turnOffAutoMerge(client, sessionId, status));
      } else if (!isTauri()) {
        setProblem("Auto-merge is turned on only from the desktop app.");
      } else if (method) {
        const why = problemOf(await turnOnAutoMerge(invoke, sessionId, status, method));
        if (why) setProblem(why);
      }
    } catch (error) {
      setProblem(message(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="pr-card" aria-label="Pull request">
      <div className="pr-head">
        <Badge tone={view.state.tone} icon={Icon}>
          {view.state.label}
        </Badge>
        <h3 className="pr-title" title={view.title}>
          {view.title}
        </h3>
        <span className="pr-ref muted">{view.ref}</span>
        {view.link && (
          <button type="button" className="ghost small" onClick={() => void openExternal(view.link!)} title="Open on GitHub in your browser">
            <ExternalLink aria-hidden="true" /> Open
          </button>
        )}
        <button
          ref={toggle}
          type="button"
          className="ghost small"
          aria-expanded={open}
          aria-controls={detailsId}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />} Details
        </button>
      </div>

      <ul className="pr-facts" aria-label="Status">
        <li className={`pr-fact tone-${view.checks.tone}`}>
          {view.checks.ok !== undefined && <ToolState ok={view.checks.ok} />}
          <span>{view.checks.label}</span>
        </li>
        {!view.ended && (
          <>
            <li className={`pr-fact tone-${view.review.tone}`}>{view.review.label}</li>
            <li className={`pr-fact tone-${view.merge.tone}`}>{view.merge.label}</li>
            {view.mergeNote && <li className="pr-fact tone-neutral">{view.mergeNote}</li>}
          </>
        )}
      </ul>

      {view.checks.failing.length > 0 && (
        <ul className="pr-failing" aria-label="Failing checks">
          {view.checks.failing.map((name, index) => (
            <li key={index}>{name}</li>
          ))}
          {view.checks.more > 0 && <li className="muted">and {view.checks.more} more</li>}
        </ul>
      )}

      {view.error && (
        <Notice tone="warn" title="GitHub could not be read just now">
          {view.error}. This is the last status that was read{when ? `, ${when === "now" ? "just now" : `${when} ago`}` : ""}.
        </Notice>
      )}

      {view.untrusted && (
        <Notice tone="warn" title="Untrusted comment text was handled">
          {view.untrusted}{" "}
          <button type="button" className="ghost small" disabled={busy || !client} onClick={() => void changeMonitor({ ack_untrusted: true })}>
            Got it
          </button>
        </Notice>
      )}
      {view.ignored && <p className="muted pr-ignored">{view.ignored}</p>}

      {open && (
        <div id={detailsId} className="pr-details">
          {view.branches && <p className="muted pr-branches">{view.branches}</p>}
          <Controls
            view={view}
            method={method}
            onMethod={setChosen}
            busy={busy || !client}
            onToggleAutoFix={() => void changeMonitor({ auto_fix: !view.autoFix.on })}
            onToggleAutoMerge={() => void toggleAutoMerge()}
          />
          {problem && <Notice tone="danger">{problem}</Notice>}
          {client && (
            <div className="pr-actions">
              <button type="button" className="ghost small" onClick={() => void unbind()}>
                {view.autoMerge.on ? "Stop following and turn auto-merge off" : "Stop following this pull request"}
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

/**
 * The two switches and the method picker. The auto-fix switch can be turned on while the pull
 * request is open and off at any time, with what it does beside it. The auto-merge switch is on when GitHub has the request, can be turned on when nothing says it
 * cannot (the note beside it says what), and can always be turned off.
 */
function Controls({
  view,
  method,
  onMethod,
  busy,
  onToggleAutoFix,
  onToggleAutoMerge,
}: {
  view: PrCardView;
  method: PrMergeMethod | null;
  onMethod: (method: PrMergeMethod) => void;
  busy: boolean;
  onToggleAutoFix: () => void;
  onToggleAutoMerge: () => void;
}) {
  const fixId = useId();
  const mergeId = useId();
  return (
    <div className="pr-controls">
      <div className="pr-control">
        <label className="option-row">
          <Switch
            checked={view.autoFix.on}
            onChange={onToggleAutoFix}
            label={`${view.autoFix.label}. ${view.autoFix.reason}`}
            disabled={busy || !(view.autoFix.canTurnOn || view.autoFix.canTurnOff)}
          />
          <span>{view.autoFix.label}</span>
        </label>
        <p className="muted" id={fixId}>
          {view.autoFix.hint ?? view.autoFix.reason}
        </p>
      </div>
      <div className="pr-control">
        <label className="option-row">
          <Switch
            checked={view.autoMerge.on}
            onChange={onToggleAutoMerge}
            label={`${view.autoMerge.label}. ${view.autoMerge.reason}`}
            disabled={busy || !(view.autoMerge.canTurnOn || view.autoMerge.canTurnOff)}
          />
          <span>{view.autoMerge.label}</span>
        </label>
        <p className="muted" id={mergeId}>
          {view.autoMerge.hint ?? view.autoMerge.reason}
        </p>
        <div className="pr-method">
          <span className="muted" aria-hidden="true">
            Merge method
          </span>
          <Select
            size="small"
            value={method ?? ""}
            options={view.methods}
            onChange={(value) => onMethod(value as PrMergeMethod)}
            inert={!view.autoMerge.canTurnOn || busy}
            aria-label="Merge method"
            aria-describedby={mergeId}
            placeholder="Unknown"
          />
        </div>
        {view.methodNote && <p className="muted">{view.methodNote}</p>}
      </div>
    </div>
  );
}

/** The empty state: no pull request is bound, so a one-line prompt that opens the bind field. */
function BindForm({
  sessionId,
  client,
  focusLink,
  onFocused,
  onBound,
}: {
  sessionId: string;
  client: DaemonClient;
  /** Focus the link once, as the card has just returned to its empty state. */
  focusLink: boolean;
  onFocused: () => void;
  onBound: (status: PrStatus) => void;
}) {
  const [open, setOpen] = useState(false);
  const link = useRef<HTMLButtonElement>(null);
  const returnFocus = useRef(focusLink);
  // After Cancel or an ended binding, the link the user came from takes focus back.
  useEffect(() => {
    if (open || !returnFocus.current) return;
    returnFocus.current = false;
    link.current?.focus();
    onFocused();
  }, [open]);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const inputId = useId();
  const errorId = useId();

  if (!open) {
    return (
      <p className="pr-card pr-empty muted">
        <GitPullRequest aria-hidden="true" />
        <span>No pull request is bound to this session.</span>
        <button ref={link} type="button" className="link" onClick={() => setOpen(true)}>
          Bind a pull request
        </button>
      </p>
    );
  }

  const submit = async () => {
    const parsed = parseBindInput(text);
    if (!parsed.ok) {
      setProblem(parsed.reason);
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      onBound(await client.call<PrStatus>("pull_requests.bind", { session_id: sessionId, url: parsed.url }));
    } catch (error) {
      setProblem(message(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="pr-card pr-bind"
      aria-label="Bind a pull request"
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <label htmlFor={inputId}>Pull request address</label>
      <div className="pr-bind-row">
        <input
          id={inputId}
          type="url"
          autoFocus
          value={text}
          placeholder="https://github.com/owner/repo/pull/12"
          aria-invalid={problem ? true : undefined}
          aria-describedby={problem ? errorId : undefined}
          onChange={(event) => setText(event.target.value)}
        />
        <button type="submit" className="primary small" disabled={busy}>
          {busy ? "Binding…" : "Bind"}
        </button>
        <button
          type="button"
          className="ghost small"
          onClick={() => {
            returnFocus.current = true;
            setOpen(false);
          }}
        >
          Cancel
        </button>
      </div>
      {problem && (
        <p className="error" id={errorId} role="alert">
          {problem}
        </p>
      )}
      <p className="muted">It must be a pull request in this session's repository.</p>
    </form>
  );
}
