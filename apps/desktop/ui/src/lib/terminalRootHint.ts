/**
 * Spots output that says a command needed administrator rights. The server terminal runs as the
 * server's service user (no root, no sudo), so a person who types `apt install` or a system-wide
 * `npm install -g` gets an error that does not say what to do; the view adds a one-line hint.
 * Pure logic, no DOM: the view feeds it every chunk of output.
 */

// Every ANSI escape sequence (colours, cursor moves) and bare control character, so a pattern sees
// the words the way the person reads them.
// eslint-disable-next-line no-control-regex
const ESCAPES = /\u001b\[[0-9;?]*[ -/]*[@-~]|\u001b\][^\u0007]*\u0007|[\u0000-\u0008\u000b-\u001f\u007f]/g;

/** What a command prints when it needs root (apt, dnf, systemctl, sudo, npm into a system folder). */
const NEEDS_ROOT: readonly RegExp[] = [
  /could not open lock file/i,
  /unable to acquire the dpkg/i,
  /are you root\?/i,
  /superuser privileges/i,
  /(?:must|need to|needs to|has to|have to) be (?:run )?(?:as|with|by|under|the) (?:the )?(?:root|superuser)/i,
  /you (?:must|need to) be root/i,
  /(?:requires?|need(?:s)?) root (?:privileges|access|permissions?)/i,
  /interactive authentication required/i,
  /is not in the sudoers file/i,
  /sudo: (?:a password is required|a terminal is required|command not found|not found)/i,
  // A system folder refusing a write: npm's global folder (`EACCES ... /usr/lib/node_modules`), cp, mv, tee.
  // Folders under /var/lib count only for the package managers, never for the service user's own data.
  /EACCES[\s\S]{0,300}?[\s'"]\/(?:usr|opt|etc|var\/lib\/(?:dpkg|apt|dnf|rpm))\//,
  /permission denied[^\n]{0,80}[\s'"]\/(?:usr|opt|etc|var\/lib\/(?:dpkg|apt|dnf|rpm))\//i,
  /[\s'"]\/(?:usr|opt|etc|var\/lib\/(?:dpkg|apt|dnf|rpm))\/[^\n]{0,160}: permission denied/i
];

/** How much recent text is kept, so a message split over two chunks (or two lines) still matches. */
const WINDOW = 600;

export class RootHintDetector {
  private tail = "";
  private readonly decoder = new TextDecoder("utf-8", { fatal: false });

  /** Feed the next output bytes; `true` when they complete a "needs root" message. */
  feed(bytes: Uint8Array): boolean {
    this.tail = (this.tail + this.decoder.decode(bytes, { stream: true })).replace(ESCAPES, "").slice(-WINDOW);
    if (!NEEDS_ROOT.some((pattern) => pattern.test(this.tail))) {
      return false;
    }
    // The same message is not reported again by the next chunk.
    this.tail = "";
    return true;
  }
}
