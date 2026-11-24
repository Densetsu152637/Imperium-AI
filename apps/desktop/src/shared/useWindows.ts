// Which windows and modes lie over the workspace: Settings, Connectors, the model dialog, an
// environment's window, and the panel editor. Two keys close or open them (App.tsx draws them).

import { useEffect, useState, type RefObject } from "react";

import type { ModelDialogTarget } from "../models/ModelConfigDialog";
import type { AppSettings } from "../settings/settings";
import { isCapturing, triggers } from "../settings/shortcuts";
import { anyModalOpen } from "../tour/tourSteps";

/**
 * Gives the document keyboard focus once the app has rendered, so Ctrl+, and Ctrl+K work at launch
 * without a click first. A web view starts with no focused element and, in some shells, without
 * document focus; the window's key listeners only hear keys while the document has it. Focus moves
 * to the app's root element (focusable, no ring), which also leaves Tab starting at the title bar.
 * An already focused element, such as an autofocused field, keeps its focus.
 */
export function useLaunchFocus(root: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    // Ask for document focus whatever is focused: an autofocused field has focus within a document that has none.
    window.focus();
    const element = root.current;
    const idle = !document.activeElement || document.activeElement === document.body || document.activeElement === document.documentElement;
    if (element && idle) element.focus({ preventScroll: true });
  }, [root]);
}

export function useWindows(isDetached: boolean, shortcuts: AppSettings["shortcuts"]) {
  /** The environment window: which environment it edits (null id: a new one). */
  const [envWindow, setEnvWindow] = useState<{ id: string | null; confirmDelete?: boolean } | null>(null);
  /** The panel editor: panels are moved only while it is on. */
  const [editingLayout, setEditingLayout] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  /** The Settings section the command palette asked for. */
  const [settingsRequest, setSettingsRequest] = useState<{ section: string } | null>(null);
  const [modelDialog, setModelDialog] = useState<ModelDialogTarget | null>(null);
  const [showConnectors, setShowConnectors] = useState(false);
  const [showPlugins, setShowPlugins] = useState(false);

  // Esc leaves the panel editor, unless a dialog is open (it closes that instead).
  useEffect(() => {
    if (!editingLayout) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || anyModalOpen()) return;
      setEditingLayout(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editingLayout]);

  // The Settings shortcut (Ctrl+, by default; Cmd+, on macOS) opens it, as in most desktop apps.
  useEffect(() => {
    if (isDetached) return;
    const onKey = (event: KeyboardEvent) => {
      if (!isCapturing() && triggers(event, shortcuts, "settings.toggle")) {
        event.preventDefault();
        setShowSettings((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isDetached, shortcuts]);

  return {
    envWindow,
    setEnvWindow,
    editingLayout,
    setEditingLayout,
    showSettings,
    setShowSettings,
    settingsRequest,
    setSettingsRequest,
    modelDialog,
    setModelDialog,
    showConnectors,
    setShowConnectors,
    showPlugins,
    setShowPlugins,
  };
}
