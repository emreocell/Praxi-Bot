# Windows command sandbox spike

Issue: #572. Status: experimental, opt-in, never used by the daemon's shell tool.
Design-first draft: PR #634, opened before implementation.

## Implemented boundary

Use a privilege-restricted primary token, an AppContainer without network
capabilities by default, and a job object with kill-on-close and a memory cap.
Create the child suspended, attach it to the job, and only then resume it.
Setup failure stops the command; there is no unrestricted fallback.

The first experiment runs `cmd.exe /d /s /c` in a newly created disposable
workspace. Requiring a new directory avoids changing permissions on an existing
project, traversing junctions, or granting access through existing hard links.
Recursive project import, interactive terminals, PowerShell and developer tools
are outside this spike. Both a Cargo feature and an explicit configuration flag
must be enabled. The existing permission engine remains mandatory for any future
integration; this experiment does not add a daemon route.

## Threat model and limits

The child command is untrusted; the caller, Windows kernel, installed system
runtime and workspace parent are trusted. Protect ordinary private files outside
the workspace, keep descendants in the job, and deny network by default. This is
not a virtual machine or a promise that every byte outside the workspace is
unreadable: Windows runtime resources and resources explicitly accessible to
AppContainers remain exceptions. The implementation requires LPAC to remove
the broad ALL APPLICATION PACKAGES grant; unsupported systems fail closed.
Resources granted to ALL RESTRICTED APPLICATION PACKAGES or the selected
Internet capability can still be accessible according to their Windows ACLs.
This is ACL confinement, not a filesystem namespace or an exact path allowlist.
AppContainer profile storage and registry resources are also exceptions. No
claims cover kernel exploits, malicious administrators, hostile
same-user processes, disk/CPU quotas, or secrets deliberately placed in the
workspace/command. Network allowance is broad, not a domain allowlist.

## Reference studied

Local reference: `faydali-repo/openai-codex`, Apache-2.0 (`LICENSE`), especially
`codex-rs/windows-sandbox-rs/src/token.rs`, `process.rs`, and
`unified_exec/backends/legacy.rs`. Its `WRITE_RESTRICTED` token only checks
restricting SIDs for writes; copying that alone would not establish read
isolation. Job containment is a separate concern from file access. This spike
uses Windows APIs directly rather than importing that implementation or its
elevated setup service. Reference revision:
`1be4eb123e5f870cfdee701dee171801cab39c66`. Also studied
`codex-rs/utils/pty/src/win/job.rs`; this spike omits its breakaway flag.

Primary API references:

- [Restricted tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens)
- [Launch an AppContainer, including LPAC](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)
- [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)

## Running the experiment

Create a trusted JSON configuration outside the future workspace:

```json
{"enabled": true, "allow_network": false, "memory_limit_bytes": 268435456, "timeout_ms": 30000}
```

On Windows, from the repository, with the normal worker Cargo environment set:

```powershell
cargo run -p windows-sandbox-spike --features windows-sandbox --example run-command -- D:/sandbox-config.json D:/new-disposable-workspace 'echo hello >result.txt'
```

The directory must **not exist**; its parent must exist on a fixed local drive.
The library returns the shell exit code. There are no inherited standard handles
or interactive console; redirect output into workspace files. The caller owns
removing that directory after inspecting results. Both Cargo feature and config
flag default to off. A missing feature/platform, disabled config, unsupported
Windows policy, invalid input or setup failure starts no unrestricted command.
Do not add an automatic approval or an unrestricted retry around this API.

The parent must remain trusted while creating the workspace and running. This is not protection
against another process running as the same user swapping paths concurrently.
Low-integrity workspace permissions allow the confined process to change files
there; do not put sensitive input in that directory. Only `LOCALAPPDATA` (required
by
Windows profile lookup), a system-derived `SystemRoot`, and workspace `TEMP`/`TMP`
are supplied; no API credentials, proxy variables, PATH or shell startup hooks
are inherited. `cmd /d` disables AutoRun. The executable comes from the Windows
system-directory API, not COMSPEC or PATH.

The job caps aggregate committed memory (256 MiB by default), disallows
breakaway, and kills descendants when its last noninherited handle closes.
The shell starts suspended, so failure assigning it to the job terminates it
before it can execute. A deadline of 30 seconds is the default (maximum five
minutes). Memory exhaustion can surface as an application allocation failure,
not a friendly quota message. Disk space, CPU rate and output size are not capped.
A unique AppContainer profile is removed after execution; abrupt parent death
still closes the job, but may leave profile storage behind. Cleanup errors on the
normal completion path are reported; error-unwind cleanup is best effort.

`allow_network: true` grants only Internet client capability. It is broad outbound
Internet permission, not a host/port allowlist, and does not grant a loopback
exemption, private-network/server capabilities or a firewall bypass. No firewall,
loopback exemption, existing project ACL or system ACL is modified.

## Plain wording for a future user interface

No UI is wired in this spike. Proposed wording, with actual translations:

- **English:** Experimental Windows sandbox. The command can use its temporary
  workspace. Private files outside it and network connections are blocked by
  default. Some Windows resources and sandbox storage remain accessible. This
  does not protect secrets you place in the workspace. If setup fails, the
  command will not run.
- **Turkish:** Deneysel Windows korumalı alanı. Komut geçici çalışma klasörünü
  kullanabilir. Bu klasörün dışındaki özel dosyalara erişim ve ağ bağlantıları
  varsayılan olarak engellenir. Bazı Windows kaynaklarına ve korumalı alanın
  depolamasına erişim sürebilir. Çalışma klasörüne koyduğunuz gizli bilgileri
  korumaz. Kurulum başarısız olursa komut çalıştırılmaz.
- **German:** Experimentelle Windows-Sandbox. Der Befehl kann seinen temporären
  Arbeitsordner verwenden. Der Zugriff auf private Dateien außerhalb dieses
  Ordners und Netzwerkverbindungen sind standardmäßig gesperrt. Einige
  Windows-Ressourcen und der Sandbox-Speicher bleiben zugänglich. Geheimnisse,
  die Sie im Arbeitsordner ablegen, sind nicht geschützt. Schlägt die Einrichtung
  fehl, wird der Befehl nicht ausgeführt.

## Verification and compatibility

Run the focused Windows tests with:

```powershell
cargo test -p windows-sandbox-spike --features windows-sandbox -- --include-ignored
```

The canary test first proves an ordinary shell can read the private outside file,
then checks denied reads and writes alongside successful workspace read/write.
It also rejects reusing an existing workspace. The timeout test runs a bounded
infinite shell loop. The dedicated Windows CI workflow enables the feature;
ordinary workspace builds
leave it off. Native tests need Windows AppContainer support and a working
desktop/profile, not elevation, real credentials or public Internet access. They
passed locally, but the first hosted Windows CI run exited with
`STATUS_DLL_INIT_FAILED` (0xc0000142) before either command ran. As allowed by the
spike scope, those two native tests are explicitly `#[ignore]` with that reason;
use `--include-ignored` locally. Hosted CI compiles them and runs the config
tests, and is **not** evidence of native confinement. See the failed
[hosted run](https://github.com/emreocell/Praxi-Bot/actions/runs/45066073994).
The runner reports this startup status as an unsupported desktop/profile rather
than exposing an unexplained numeric exit code. Private-desktop/window-station
setup needs a separate design; no access to an existing desktop was broadened.
Desktop, keyboard and GUI isolation are not certified by this spike.

Observed during development: stripping `LOCALAPPDATA` caused process creation to
fail with Windows error 203. Keeping that one path variable fixed profile lookup.
Rust canonical paths with a verbatim drive prefix made `cmd` choose the wrong
working directory; the runner normalizes that prefix after validating the local
path. LPAC `cmd` builtins work, while this machine's system `curl` did not start
successfully under LPAC. A separate socket probe copied into the workspace was
also denied before
startup. Neither failure is counted as a successful socket-denial test. Direct
network enforcement was not verified; it currently relies on Windows enforcing
the zero-capability LPAC token. No failing test was ignored to suggest otherwise.
PowerShell, package managers, compilers, installers, GUI tools,
UNC/mapped drives and long-path compatibility are not certified. Unsupported
tools must fail visibly; do not grant more capabilities or run them unrestricted
as an automatic fallback. A dedicated account/service or VM is the next design
step if general developer-tool compatibility is required.

Not verified by these tests: external Internet allowance/denial across IPv4/IPv6,
DNS and broker behavior, memory-pressure stress, descendant-race stress, crash
profile cleanup, every Windows release and enterprise policy, or security against
kernel/administrator/same-user attacks. This spike is not production-ready.
