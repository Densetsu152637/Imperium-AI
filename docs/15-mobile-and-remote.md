# 15 — Mobile app and remote sessions

A phone app lets users follow and drive sessions running on their own computers. The computer
does all the work; the phone is a client of its daemon, like the desktop app.

## Scope

- List paired computers, their projects, and sessions; see live status and telemetry.
- Read and reply to sessions, start new sessions from a project, stop or steer agents.
- Approve or deny queued permission requests from [project agents](14-projects-and-automation.md#project-agent).
- Receive notifications: session finished, approval needed, schedule failed, budget reached.
- View artefacts (files, diffs, images, audio). Not in scope: running models on the phone, or
  editing large codebases on the phone.

## Pairing

Pairing is per computer:

1. In the desktop app, *Settings → Devices → Pair phone* shows a QR code containing the
   computer's public key, a one-time pairing secret, and reachable addresses.
2. The phone scans it, generates its own key pair, and completes a mutually authenticated key
   exchange (Noise `XX` pattern or equivalent).
3. The computer stores the phone's public key; the phone stores the computer's. Either side
   can revoke the pairing. Each paired device gets scoped permissions (read-only, interact,
   approve).

## Connectivity

All traffic is end-to-end encrypted between the phone and the daemon, whatever the path.

| Path                    | When                                            | Notes                                  |
| ----------------------- | ----------------------------------------------- | -------------------------------------- |
| Direct (LAN)            | Same network                                    | Discovered via mDNS                    |
| Private overlay network | Tailscale, ZeroTier, WireGuard already in use   | Recommended for away-from-home use     |
| Relay                   | Neither of the above                            | Self-hostable relay forwards encrypted frames only; it cannot read them. A hosted relay is a later, optional decision |

The daemon's remote listener is off until the first phone is paired and only accepts paired
keys.

## Notifications

Push notifications need a relay to Apple/Google push services; payloads carry only an opaque ID,
and the phone fetches details over the encrypted channel. Without push, the app shows
notifications while open or via periodic background fetch where the OS allows.

## Technology

- **Tauri 2 mobile** (iOS and Android) reusing the React UI packages and the generated protocol
  types, with a phone-specific layout. React Native is the fallback if Tauri mobile proves
  insufficient (see [decisions](13-decisions.md)).
- Distribution: Android via GitHub Releases (APK) and Google Play; iOS via TestFlight, then the
  App Store. Both stores need developer accounts and signing keys held as CI secrets.
