/**
 * What the app does while a paired always-on host is the selected computer.
 *
 * Pure helpers: the local first-run wizard never runs against a host, the host's refusals
 * (`crates/daemon-api/src/peer.rs`) are shown as one human sentence by `humanError`, and the entry point for
 * "connect a model on this server" sits behind a single flag.
 */

/** Same id as `LOCAL_COMPUTER` in the daemon context (kept literal so this file stays pure). */
const LOCAL = "local";

/**
 * Sending a provider key from this computer to a host is NOT built yet (owner approval pending). While this is `false` the "Connect a model on this server" row only explains the
 * command to run on the server. The real flow is enabled by flipping this flag and rendering the
 * key form where `HostEmptyState` renders `HostModelDialog`.
 */
export const HOST_MODEL_KEY_FROM_APP = false;

/** What to run in a terminal on the server to connect a model there. */
export const HOST_MODEL_SETUP_COMMAND = "bot setup";

/** The local first-run wizard only ever talks to this computer's own daemon. */
export function onboardingAllowed(activeComputerId: string): boolean {
  return activeComputerId === LOCAL;
}

/**
 * Host refusal messages (daemon `403` bodies) mapped to i18n keys under `hostMode.refusal`.
 * Matched on the English text the daemon sends; the daemon strings are not shown to people.
 */
const REFUSALS: ReadonlyArray<readonly [needle: string, key: string]> = [
  ["execution policy is managed on the host", "policy"],
  ["secrets are managed on the host", "secrets"],
  ["tool servers and skills are managed on the host", "tools"],
  ["mcp server management is a host-local", "tools"],
  ["mcp sign-in is a host-local", "tools"],
  ["account credentials are managed on the host", "credentials"],
  ["host transfer and channel pairing are done on the host", "hostTransfer"],
  ["pairing and host lifecycle are managed on the host", "lifecycle"],
  ["permissions and tools of a bot on a host are managed on the host", "botPermissions"],
  ["knowledge settings are managed by the local owner", "knowledge"],
  ["composio connections are managed by the owner", "composio"],
  ["only the local user answers overlay questions", "overlay"],
  ["only the local user may do this", "localOnly"],
  ["only the local user can clear it", "localOnly"],
  ["the local user stopped the takeover", "localOnly"],
  ["screen:input denied", "screenInput"],
  ["screen:view denied", "screenInput"],
  ["input grant for this device", "screenInput"],
  ["host-local provider management is not allowed", "localProviders"],
  ["connection tests spend this host", "connectionTest"],
  ["local-owner only", "ownerOnly"],
  ["peer role does not allow this operation", "role"],
  ["paired peer identity required", "role"]
];

/** i18n key (under `hostMode.refusal.`) for a refusal, or `null` when it is not one. */
export function hostRefusalKey(status: number | undefined, message: string): string | null {
  const text = message.toLowerCase();
  for (const [needle, key] of REFUSALS) {
    if (text.includes(needle)) {
      return key;
    }
  }
  return status === 403 ? "generic" : null;
}

/** What a paired desktop may do on a host today (`peer.rs`), for the empty-state note. */
export const HOST_DESKTOP_MAY = ["bots", "chats", "screen"] as const;
