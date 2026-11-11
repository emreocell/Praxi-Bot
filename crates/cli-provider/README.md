# CLI provider launch boundary

Praxi treats prompt injection, a provider CLI's native tools, and processes
belonging to other OS users as threats. A malicious process already running
as the same Windows user is outside that containment promise: it already has
access to that user's files, browser sessions and Credential Manager, and may
be able to read or modify Praxi itself. These controls are defense in depth;
they do not turn a same-user process into a separate security principal.

## Windows defense in depth

- A turn canonicalizes the resolved executable and opens it with
  `FILE_SHARE_READ` only before the first version probe. The handle remains
  open across the version and help probes until `CreateProcess` and the
  suspended-child job assignment return. New write/delete handles, including
  executable renames, are denied during that interval. An existing incompatible
  handle fails the launch closed. Identity is checked immediately after help
  as well as after version probing. This pins the executable, not every DLL,
  interpreter argument, or ancestor directory against a same-user attacker.
- The suspended root is assigned to a kill-on-close job using a short-lived
  all-access handle. Before resuming the root, that handle is duplicated with
  only `JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE | SYNCHRONIZE`, then closed.
  The bridge retains the reduced, non-inheritable handle. It can check socket
  owner membership and terminate the tree; it cannot add a new root or change
  job limits. Real descendants still inherit membership normally.
- No child-process DACL change is made. Denying the user's process rights can
  break debuggers, updaters, and supported CLI behavior, and needs separate
  measurement and compatibility evidence. A same-user attacker remains outside
  the threat model even with the reduced job handle.

Codex remains never-spawned while its native `unified_exec` cannot be disabled.
The detector and turn runner report the existing non-retryable refusal.

The platform behavior is defined by Microsoft's
[DuplicateHandle](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle)
and [file-sharing rules](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew).
In particular, handle reduction is not a promise that an attacker controlling
the owning process cannot obtain other rights.
