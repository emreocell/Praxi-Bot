# Stop cancels the run

Pressing Stop in the chat composer cancels the run that is actually in
flight. The run leaves `running` within about a second. It does not stay
`running` after Stop.

## What Stop does

- The daemon cancel route signals the run. The agent loop also arms a
  supervisor: if the run is still not finished one second later, the
  supervisor aborts the drive task and marks the run `cancelled`. That covers
  a tool that blocks the worker without ever polling the cancel token.
- The in-flight model request is aborted. A retry or a fallback model is not
  started once Stop has been pressed. Dropping the stream drops the request.
- The in-flight tool is dropped. A shell the run started is killed when its
  task is dropped. A browser the run launched is killed when that child is
  dropped. A page load that is dropped mid-wait sends `Page.stopLoading`. A
  load that merely times out is left alone.
- Every approval, question or secret request the run (and each descendant)
  has open is closed as `denied` inside Stop itself, before the stop marker can
  be cleared: one approval row, one `approval.denied` audit entry and one
  `tool_call_denied` event each, never `expired` and never `approved`. This
  holds for the first press, the hard stop (second press, latch-parked run or
  the one-second supervisor) and for a card parked by a parallel call after
  Stop. A timeout, and a schedule run that pauses because nobody answered,
  still use `expired`.
- After Stop no approval is accepted. The closed cards leave the pending map,
  so approve, answer and secret-request completion for them fail as "no such
  pending approval". A click that was already sent loses too: the final check
  before a tool starts (`ExecPermit::commit`, under `exec_gate`) and the
  post-approve recheck fail closed when the run is stopped or `Cancelled`.
  No standing grant is minted and no tool starts for a stopped run.
- Every descendant is covered by its own one-second supervisor. A subagent
  stuck in a tool that never polls the cancel token is aborted and finished as
  `Cancelled` even when its parent already ended.
- Messages the user already queued stay queued. Stop does not start them and
  does not delete them. The chat is free for a later send. The composer does
  not ask "Send queued messages now?".
- The activity line says "Stopped by you".

## What the composer shows

- The round button switches to Stopping as soon as Stop is pressed.
- A second press while it says Stopping is the hard stop.
- When the event stream has finished the run, the button is Send again, even
  if the last run list still says `queued`, `running`, or `waiting_approval`.
- Text in the box still makes the button Send, so the user can queue another
  message. Queued bubbles do not hide Stop while the box is empty.
