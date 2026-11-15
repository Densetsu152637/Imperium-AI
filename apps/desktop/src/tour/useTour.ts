// The first-run tour: whether it is showing, whether it started by itself, and the first-run
// start once the interface is up (App.tsx draws it).

import { useEffect, useRef, useState } from "react";

import type { AppSettings, ChangeSettings } from "../settings";
import { anyModalOpen, autoStartReady, recordOutcome, shouldAutoStart, type TourOutcome } from "../tourSteps";

/** How long the tour waits for the composer to appear before starting without it. */
const TOUR_COMPOSER_WAIT_MS = 8000;

export function useTour({
  settings,
  isDetached,
  connected,
  approvalsWaiting,
  changeSettings,
  closeWindows,
}: {
  settings: AppSettings;
  isDetached: boolean;
  connected: boolean;
  approvalsWaiting: boolean;
  changeSettings: ChangeSettings;
  /** Closes the windows over the workspace, so the tour starts on a clear view. */
  closeWindows: () => void;
}) {
  /** The first-run tour is showing. */
  const [touring, setTouring] = useState(false);
  /** The tour started by itself (not asked for), so it steps aside for a pending approval. */
  const [tourAuto, setTourAuto] = useState(false);
  /** Starts the tour from its first step, closing the windows over the workspace first. */
  const startTour = () => {
    closeWindows();
    setTourAuto(false);
    setTouring(true);
  };
  const endTour = (outcome: TourOutcome) => {
    setTouring(false);
    changeSettings((current) => recordOutcome(current, outcome));
  };
  // A first run: the tour starts by itself, in the main window only, once connected and the
  // interface is up (the composer showing, or after a few seconds any panel) with no dialog open.
  // An approval waiting is never held up by the tour: it steps aside until none is pending
  // (and does not start while one is), then carries on where it was.
  const approvalsWaitingRef = useRef(approvalsWaiting);
  approvalsWaitingRef.current = approvalsWaiting;
  const tourPending = shouldAutoStart(settings) && !touring;
  useEffect(() => {
    if (isDetached || !tourPending || !connected) return;
    const since = Date.now();
    const timer = window.setInterval(() => {
      const composer = !!document.querySelector("form.composer");
      const anyPanel = !!document.querySelector("[data-panel]");
      const ready = autoStartReady(
        { tour: null },
        {
          mainWindow: true,
          connected: true,
          uiReady: composer || (anyPanel && Date.now() - since > TOUR_COMPOSER_WAIT_MS),
          modalOpen: approvalsWaitingRef.current || anyModalOpen(),
        },
      );
      if (ready) {
        window.clearInterval(timer);
        setTourAuto(true);
        setTouring(true);
      }
    }, 500);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tourPending, connected]);
  return { touring, tourAuto, startTour, endTour };
}
