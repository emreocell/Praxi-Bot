import assert from "node:assert/strict";
import { test } from "node:test";
import { RootHintDetector } from "./terminalRootHint.ts";

const enc = (s: string) => new TextEncoder().encode(s);

test("the messages of a command that needs root are recognised", () => {
  const needsRoot = [
    // apt as a normal user
    "E: Could not open lock file /var/lib/dpkg/lock-frontend - open (13: Permission denied)\nE: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), are you root?\n",
    "E: Could not open lock file /var/lib/apt/lists/lock - open (13: Permission denied)\n",
    // dnf / yum
    "Error: This command has to be run with superuser privileges (under the root user on most systems).\n",
    "You need to be root to perform this command.\n",
    "This script must be run as root\n",
    "Please run as root. This installer requires root privileges.\n",
    // systemd
    "Failed to restart praxi.service: Interactive authentication required.\n",
    // sudo
    "praxi is not in the sudoers file.\n",
    "sudo: a password is required\n",
    "bash: sudo: command not found\n",
    // npm -g into a system folder: the owner's second failure
    "npm error code EACCES\nnpm error syscall mkdir\nnpm error path /usr/lib/node_modules/opencode-ai\nnpm error errno -13\n",
    "npm ERR! Error: EACCES: permission denied, access '/usr/local/lib/node_modules'\n",
    "cp: cannot create regular file '/usr/local/bin/tool': Permission denied\n"
  ];
  for (const text of needsRoot) {
    assert.equal(new RootHintDetector().feed(enc(text)), true, text);
  }
});

test("ordinary output and errors that root would not fix are left alone", () => {
  const fine = [
    "total 8\ndrwxr-xr-x 2 praxi praxi 4096 Nov 27 10:00 .\n",
    "npm error code E404\nnpm error 404 Not Found - GET https://registry.npmjs.org/nope\n",
    "bash: npm: command not found\n",
    // a permission problem in the user's own folder is not "needs root"
    "cat: /var/lib/praxi/private.txt: Permission denied\n",
    "git@github.com: Permission denied (publickey).\n",
    "the word root appears in a path /root-of-project/src\n"
  ];
  for (const text of fine) {
    assert.equal(new RootHintDetector().feed(enc(text)), false, text);
  }
});

test("a message split over chunks and wrapped in colour codes is still found, and reported once", () => {
  const detector = new RootHintDetector();
  assert.equal(detector.feed(enc("\u001b[1;31mE: Could not open lo")), false);
  assert.equal(detector.feed(enc("ck file /var/lib/dpkg/lock\u001b[0m (13: Permission denied)\r\n")), true);
  // Later output is judged on its own: the same message is not reported again.
  assert.equal(detector.feed(enc("prompt$ ")), false);
  // A multi-byte character cut in half between chunks does not break the stream.
  const bytes = enc("öğrenci: sudo: a password is required\n");
  assert.equal(detector.feed(bytes.subarray(0, 1)), false);
  assert.equal(detector.feed(bytes.subarray(1)), true);
});
