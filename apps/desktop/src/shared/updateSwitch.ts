// What the Updates settings say about moving to the other release channel. Kept apart from the
// component so the wording is testable.

export type UpdateChannel = "main" | "experimental";

/** The confirmation text for installing a build from the other channel. */
export function switchMessage(
  installed: { channel: UpdateChannel; version: string },
  target: { channel: UpdateChannel; version: string; older: boolean },
): string {
  const lead = `This will install the ${target.channel} build ${target.version}, replacing ${installed.version} (${installed.channel}).`;
  const note = target.older
    ? ` ${target.version} is an older version number than ${installed.version}, but it is the latest ${target.channel} release; installing it moves you to the ${target.channel} channel.`
    : ` Installing it moves you to the ${target.channel} channel.`;
  return lead + note;
}

/** The status line when the chosen channel has no release. */
export function noReleaseMessage(channel: UpdateChannel): string {
  return `The ${channel} channel has no release to install yet.`;
}
