# Windows command sandbox spike

Issue: #572. Status: experimental, opt-in, never used by the daemon's shell tool.

## Proposed boundary

Use a privilege-restricted primary token, an AppContainer without network
capabilities by default, and a job object with kill-on-close and a memory cap.
Create the child suspended, attach it to the job, and only then resume it.
Setup failure stops the command; there is no unrestricted fallback.

The first experiment runs `cmd.exe /d /s /c` in a newly created disposable
workspace. Requiring a new directory avoids changing permissions on an existing
project, traversing junctions, or granting access through existing hard links.
Existing project import, interactive terminals, PowerShell and developer tools
are outside this spike. Both a Cargo feature and an explicit configuration flag
must be enabled. The existing permission engine remains mandatory for any future
integration; this experiment does not add a daemon route.

## Threat model and limits

The child command is untrusted; the caller, Windows kernel, installed system
runtime and workspace parent are trusted. Protect ordinary private files outside
the workspace, keep descendants in the job, and deny network by default. This is
not a virtual machine or a promise that every byte outside the workspace is
unreadable: Windows runtime resources and resources explicitly accessible to
AppContainers remain exceptions. The implementation will use LPAC to remove
the broad ALL APPLICATION PACKAGES grant if the target Windows installation
supports it. AppContainer profile storage and registry resources are also
exceptions. No claims cover kernel exploits, malicious administrators, hostile
same-user processes, disk/CPU quotas, or secrets deliberately placed in the
workspace/command. Network allowance is broad, not a domain allowlist.

## Reference studied

Local reference: `faydali-repo/openai-codex`, Apache-2.0 (`LICENSE`), especially
`codex-rs/windows-sandbox-rs/src/token.rs`, `process.rs`, and
`unified_exec/backends/legacy.rs`. Its `WRITE_RESTRICTED` token only checks
restricting SIDs for writes; copying that alone would not establish read
isolation. Job containment is a separate concern from file access. This spike
uses Windows APIs directly rather than importing that implementation or its
elevated setup service.

Primary API references:

- [Restricted tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens)
- [Launch an AppContainer, including LPAC](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)
- [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)

## Verification plan

A Windows integration test must first demonstrate that an ordinary command can
read a private outside canary, then prove the confined command cannot, while
also proving it actually executed and could write/read its workspace. Test
network denial against a local listener, without contacting an external service.
Report actual local results and any CI limitations before promoting the draft.
