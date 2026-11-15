// The settings the window shows as plain fields, by section.

import { ACCENTS, type AppSettings } from ".";

type Control =
  | { type: "toggle" }
  | { type: "select"; options: { value: string; label: string }[] }
  | { type: "swatches"; options: { value: string; label: string; color: string }[] }
  | { type: "slider"; min: number; max: number; step: number; format: (value: number) => string };

export type Field = { key: keyof AppSettings; section: string; label: string; description: string; control: Control };

const percent = (value: number) => `${Math.round(value * 100)}%`;

const choices = (entries: [string, string][]) => entries.map(([value, label]) => ({ value, label }));

export const FIELDS: Field[] = [
  // General
  {
    key: "restoreSession",
    section: "general",
    label: "Reopen where you left off",
    description: "Select the last project and session when Imperium opens.",
    control: { type: "toggle" },
  },
  {
    key: "autoStartDaemon",
    section: "general",
    label: "Start imperium-core with Imperium",
    description: "Start the bundled daemon when the app opens and none is running.",
    control: { type: "toggle" },
  },
  // Appearance
  {
    key: "theme",
    section: "appearance",
    label: "Theme",
    description: "Light or dark surfaces; System follows the operating system.",
    control: { type: "select", options: choices([["system", "System"], ["light", "Light"], ["dark", "Dark"]]) },
  },
  {
    key: "accent",
    section: "appearance",
    label: "Accent colour",
    description: "Buttons, highlights, and the connection light.",
    control: { type: "swatches", options: Object.entries(ACCENTS).map(([value, { label, base }]) => ({ value, label, color: base })) },
  },
  {
    key: "uiScale",
    section: "appearance",
    label: "Interface scale",
    description: "The size of everything outside messages and code.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "proseScale",
    section: "appearance",
    label: "Message text",
    description: "The size of conversation text.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "codeScale",
    section: "appearance",
    label: "Code text",
    description: "The size of code blocks and tool output.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "maxProseCh",
    section: "appearance",
    label: "Line length",
    description: "The widest a line of message text gets, in characters.",
    control: { type: "slider", min: 50, max: 200, step: 5, format: (value) => `${value} ch` },
  },
  {
    key: "density",
    section: "appearance",
    label: "Density",
    description: "Spacing between and inside panels.",
    control: { type: "select", options: choices([["compact", "Compact"], ["comfortable", "Comfortable"], ["spacious", "Spacious"]]) },
  },
  {
    key: "proseFont",
    section: "appearance",
    label: "Message font",
    description: "The typeface for conversation text.",
    control: { type: "select", options: choices([["sans", "Sans-serif"], ["serif", "Serif"], ["mono", "Monospace"]]) },
  },
  {
    key: "reduceMotion",
    section: "appearance",
    label: "Reduce motion",
    description: "Turn off animations and transitions.",
    control: { type: "toggle" },
  },
  // Chat
  {
    key: "sendKey",
    section: "chat",
    label: "Send with",
    description: "Which key sends a message; the other inserts a new line.",
    control: { type: "select", options: choices([["enter", "Enter"], ["mod-enter", "Ctrl+Enter"]]) },
  },
  {
    key: "renderMarkdown",
    section: "chat",
    label: "Render Markdown",
    description: "Show agent replies formatted; off shows the raw text. Each session can still switch.",
    control: { type: "toggle" },
  },
  {
    key: "wrapCode",
    section: "chat",
    label: "Wrap long code lines",
    description: "Wrap code and tool output instead of scrolling sideways.",
    control: { type: "toggle" },
  },
  {
    key: "autoScroll",
    section: "chat",
    label: "Follow new output",
    description: "Scroll to the newest message as the agent writes, until you scroll up.",
    control: { type: "toggle" },
  },
  {
    key: "pushToTalkKey",
    section: "chat",
    label: "Push-to-talk key",
    description:
      "Hold it in a session to record, release to put the words in the message box (not sent); Esc cancels. The mic button does the same. Needs speech-to-text set up ([media.transcription]).",
    control: {
      type: "select",
      options: choices([
        ["Ctrl+Space", "Ctrl+Space"],
        ["Alt+Space", "Alt+Space"],
        ["Ctrl+Shift+Space", "Ctrl+Shift+Space"],
        ["F8", "F8"],
        ["off", "Off (mic button only)"],
      ]),
    },
  },
  // Notifications
  {
    key: "soundOnApproval",
    section: "notifications",
    label: "Sound when an agent needs approval",
    description: "A short two-note chime.",
    control: { type: "toggle" },
  },
  {
    key: "soundOnFinish",
    section: "notifications",
    label: "Sound when a session finishes",
    description: "A short falling chime when an agent stops working.",
    control: { type: "toggle" },
  },
  {
    key: "titleBadge",
    section: "notifications",
    label: "Mark the window title",
    description: "Put a dot before the title while something needs you and Imperium is in the background.",
    control: { type: "toggle" },
  },
  {
    key: "notifyOnlyInBackground",
    section: "notifications",
    label: "Only when Imperium is in the background",
    description: "Show system notifications only while no Imperium window has focus. Your system's do-not-disturb still applies.",
    control: { type: "toggle" },
  },
  {
    key: "notifyApproval",
    section: "notifications",
    label: "Notify when an agent needs approval",
    description: "A system notification when an agent asks to run a tool, asks you a question, or an action waits for approval.",
    control: { type: "toggle" },
  },
  {
    key: "notifyFinished",
    section: "notifications",
    label: "Notify when a session finishes",
    description: "A system notification when an agent finishes its turn and waits for you.",
    control: { type: "toggle" },
  },
  {
    key: "notifyFailed",
    section: "notifications",
    label: "Notify when a turn fails",
    description: "A system notification when an agent's turn stops on an error.",
    control: { type: "toggle" },
  },
  {
    key: "notifyBudget",
    section: "notifications",
    label: "Notify about budgets and usage",
    description: "A system notification when a budget reaches one of its alert thresholds (Settings → Budgets).",
    control: { type: "toggle" },
  },
  {
    key: "notifyDownload",
    section: "notifications",
    label: "Notify when a model download finishes",
    description: "A system notification when a downloaded model is ready to use.",
    control: { type: "toggle" },
  },
  {
    key: "notifyUpdate",
    section: "notifications",
    label: "Notify when an update is ready",
    description: "A system notification when a new version has downloaded and installs on restart.",
    control: { type: "toggle" },
  },
  {
    key: "notifyInitiative",
    section: "notifications",
    label: "Notify about agents' suggestions and sessions",
    description:
      "A system notification when a persistent agent with initiative suggests work, reminds you what is left, or starts a session.",
    control: { type: "toggle" },
  },
];

// ---- Daemon settings sections (config.describe groups by top-level table) ----
