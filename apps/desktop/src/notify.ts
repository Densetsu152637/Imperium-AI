// Gets the user's attention when an agent needs approval or a session finishes: a short tone and,
// while the window is in the background, a dot in front of its title. Settings → Notifications
// turns each on or off.

import type { AppSettings } from "./settings";

export type Attention = "approval" | "finished";

const TONES: Record<Attention, number[]> = {
  approval: [880, 1175],
  finished: [660, 523],
};

let audio: AudioContext | null = null;

function chime(frequencies: number[]): void {
  try {
    audio ??= new AudioContext();
    const start = audio.currentTime;
    frequencies.forEach((frequency, index) => {
      const oscillator = audio!.createOscillator();
      const gain = audio!.createGain();
      oscillator.frequency.value = frequency;
      const at = start + index * 0.14;
      gain.gain.setValueAtTime(0.0001, at);
      gain.gain.exponentialRampToValueAtTime(0.12, at + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.22);
      oscillator.connect(gain).connect(audio!.destination);
      oscillator.start(at);
      oscillator.stop(at + 0.25);
    });
  } catch {
    // No audio device: stay quiet.
  }
}

const BADGE = "● ";

function badgeTitle(): void {
  if (!document.hidden || document.title.startsWith(BADGE)) return;
  document.title = BADGE + document.title;
  const clear = () => {
    if (document.hidden) return;
    document.title = document.title.replace(BADGE, "");
    document.removeEventListener("visibilitychange", clear);
  };
  document.addEventListener("visibilitychange", clear);
}

export function notify(kind: Attention, settings: AppSettings): void {
  const sound = kind === "approval" ? settings.soundOnApproval : settings.soundOnFinish;
  if (sound) chime(TONES[kind]);
  if (settings.titleBadge) badgeTitle();
}

/** Plays a tone so the user can hear what a setting does. */
export function preview(kind: Attention): void {
  chime(TONES[kind]);
}
