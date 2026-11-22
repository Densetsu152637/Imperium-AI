// The session's browser: the page after the agent's latest action, which the user can take over.
// While the user drives, clicks on the page, typing, scrolling, and the address bar act in the
// agent's browser (`session.browser_act`) and the agent's browser tools are refused; *Give back
// to the agent* returns it. Each action returns a fresh screenshot, so the page updates on
// interaction rather than streaming. Each open tab shows in a strip above the page with its own
// latest screenshot; the user can look at any tab, and while driving can open, switch, and close
// tabs (the page the agent's tools work on is the active one).

import { ArrowLeft, ArrowRight, Plus, RotateCw, X } from "lucide-react";
import { type FormEvent, type KeyboardEvent, type MouseEvent, useCallback, useEffect, useId, useRef, useState } from "react";
import type { BrowserAction, BrowserActResult, BrowserControlResult } from "@imperium/protocol";

import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import { type TabItem, Tabs, tabPanelProps } from "../components/Tabs";
import type { DaemonClient } from "../daemon/client";
import { browserAddress, clickFraction, keyAction, shownTab } from "../sessions/preview";
import type { SessionState } from "../sessions/sessions";

/** How long typed characters are gathered before they are sent as one action. */
const TYPE_DELAY_MS = 200;

const message = (failure: unknown): string => (failure instanceof Error ? failure.message : String(failure));

export function BrowserPanel({ session, client }: { session: SessionState | null; client: DaemonClient | null }) {
  const sessionId = session?.summary.session_id ?? null;
  const mirror = session?.browserTabs ?? null;
  const driving = session?.driver === "user";
  // The tab on screen: the one the user chose to look at, or (always, while driving) the active one.
  const [chosen, setChosen] = useState<string | null>(null);
  const shown = shownTab(mirror, driving ? null : chosen);
  const view = (shown ? mirror?.pages[shown] : undefined) ?? session?.browser;
  const tabs = mirror?.tabs ?? [];
  const hasView = Boolean(view);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  // Actions go to the browser one at a time, in the order the user made them.
  const queue = useRef<Promise<void>>(Promise.resolve());
  const typing = useRef({ text: "", timer: 0 });
  const image = useRef<HTMLImageElement | null>(null);
  const tabsBase = useId();

  const act = useCallback(
    (action: BrowserAction) => {
      if (!client || !sessionId) return;
      queue.current = queue.current.then(async () => {
        try {
          await client.call<BrowserActResult>("session.browser_act", { session_id: sessionId, action });
          setError(null);
        } catch (failure) {
          setError(message(failure));
        }
      });
    },
    [client, sessionId],
  );

  const flushTyping = useCallback(() => {
    window.clearTimeout(typing.current.timer);
    const text = typing.current.text;
    typing.current.text = "";
    if (text) act({ type: "type", text });
  }, [act]);

  // Wheel scrolling over the page scrolls the page; the listener is not passive so the panel
  // behind it stays put.
  useEffect(() => {
    const target = image.current;
    if (!driving || !target) return;
    let pending = 0;
    let timer = 0;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      pending += event.deltaY;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const dy = pending;
        pending = 0;
        act({ type: "scroll", dy });
      }, 80);
    };
    target.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      window.clearTimeout(timer);
      target.removeEventListener("wheel", onWheel);
    };
  }, [driving, act, hasView]);

  // Typing still gathered when the user gives the browser back or switches session is dropped.
  useEffect(
    () => () => {
      window.clearTimeout(typing.current.timer);
      typing.current.text = "";
    },
    [sessionId, driving],
  );

  const control = async (user: boolean) => {
    if (!client || !sessionId) return;
    setWorking(true);
    try {
      await client.call<BrowserControlResult>("session.browser_control", { session_id: sessionId, user });
      setError(null);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setWorking(false);
    }
  };

  const selectTab = (id: string) => {
    if (driving) {
      flushTyping();
      act({ type: "switch_tab", tab: id });
    } else {
      setChosen(id);
    }
  };

  const click = (event: MouseEvent<HTMLImageElement>) => {
    const target = clickFraction(event.currentTarget.getBoundingClientRect(), event.clientX, event.clientY);
    if (!target) return;
    flushTyping();
    act({ type: "click", ...target });
    event.currentTarget.closest<HTMLElement>(".browser-page")?.focus();
  };

  const press = (event: KeyboardEvent<HTMLElement>) => {
    // Tab and Escape stay with the app, so the keyboard is never trapped in the page.
    if (event.key === "Tab" || event.key === "Escape") return;
    const action = keyAction(event);
    if (!action) return;
    event.preventDefault();
    if (action.type === "type") {
      typing.current.text += action.text;
      window.clearTimeout(typing.current.timer);
      typing.current.timer = window.setTimeout(flushTyping, TYPE_DELAY_MS);
    } else {
      flushTyping();
      act({ type: "key", key: action.key });
    }
  };

  const go = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const field = event.currentTarget.elements.namedItem("address");
    const url = browserAddress(field instanceof HTMLInputElement ? field.value : "");
    if (!url) {
      setError("Enter an http or https address.");
      return;
    }
    act({ type: "navigate", url });
  };

  if (!session || !client) {
    return <EmptyState className="compact" title="No browser yet" body="The browser appears here when an agent opens a page." />;
  }

  // The shared tabs; the agent's active tab carries a dot after its title.
  const tabItems: TabItem[] = tabs.map((tab) => ({
    id: tab.id,
    label: (
      <span title={tab.active ? `${tab.url} (the agent's active tab)` : tab.url}>
        {tab.title || tab.url || "New tab"}
      </span>
    ),
    badge: tab.active ? (
      <span role="img" aria-label="the agent's active tab">
        ●
      </span>
    ) : undefined,
  }));
  const shownInfo = tabs.find((tab) => tab.id === shown);

  return (
    <div className="browser">
      <div className="browser-bar">
        {driving ? (
          <>
            <span className="browser-nav">
              <IconButton icon={ArrowLeft} label="Back" onClick={() => act({ type: "back" })} />
              <IconButton icon={ArrowRight} label="Forward" onClick={() => act({ type: "forward" })} />
              <IconButton icon={RotateCw} label="Reload" onClick={() => act({ type: "reload" })} />
            </span>
            <form className="browser-address" onSubmit={go}>
              <input
                key={view?.url ?? ""}
                name="address"
                type="text"
                defaultValue={view?.url ?? ""}
                aria-label="Browser address"
                spellCheck={false}
              />
            </form>
            <button className="ghost browser-give" onClick={() => void control(false)} disabled={working}>
              Give back to agent
            </button>
          </>
        ) : (
          <>
            <span className="muted browser-who">{view ? "The agent drives." : "No page yet."}</span>
            <button className="ghost" onClick={() => void control(true)} disabled={working}>
              Take over
            </button>
          </>
        )}
      </div>
      {tabs.length > 0 && (
        <div className="browser-tabs">
          <Tabs
            className="browser-tab-list"
            items={tabItems}
            value={shown ?? ""}
            onChange={selectTab}
            label="Browser tabs"
            idBase={tabsBase}
          />
          {driving && (
            <span className="browser-tab-tools">
              {tabs.length > 1 && shownInfo && (
                <IconButton
                  icon={X}
                  label={`Close ${shownInfo.title || shownInfo.url || "tab"}`}
                  tooltip="Close tab"
                  onClick={() => act({ type: "close_tab", tab: shownInfo.id })}
                />
              )}
              <IconButton icon={Plus} label="New tab" onClick={() => act({ type: "new_tab" })} />
            </span>
          )}
        </div>
      )}
      {driving && (
        <Notice tone="info">You are driving. The agent's browser tools are unavailable until you give the browser back.</Notice>
      )}
      {error && <Notice tone="danger">{error}</Notice>}
      {view ? (
        <figure className="browser-view" {...(shown && tabs.length > 0 ? { ...tabPanelProps(tabsBase, shown), tabIndex: -1 } : {})}>
          <figcaption title={view.url}>
            <strong>{view.title || "Untitled"}</strong> <span className="muted">{view.url}</span>
          </figcaption>
          <div className="browser-page" tabIndex={driving ? 0 : -1} onKeyDown={driving ? press : undefined}>
            <img
              ref={image}
              className={driving ? "driving" : undefined}
              src={`data:image/jpeg;base64,${view.screenshot}`}
              alt={`Screenshot of ${view.title || view.url}`}
              onClick={driving ? click : undefined}
              draggable={false}
            />
          </div>
        </figure>
      ) : (
        <EmptyState
          className="compact"
          title="No page open"
          body="The browser appears here when an agent opens a page, or take it over to open one."
        />
      )}
    </div>
  );
}
