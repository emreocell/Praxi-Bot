# scheduler

The scheduler keeps track of when Praxi-Bot tasks are due. A `ScheduleSpec`
describes a cron expression, fixed interval, or one-off time. The timer loop
stores progress in SQLite so it can apply a schedule's `MisfirePolicy` after the
daemon restarts or the system clock jumps.

## Entry points

- [`Schedule`, `ScheduleSpec`, and `MisfirePolicy`](src/model.rs) describe the
  persisted schedule, its timing, and what to do about missed occurrences.
- [`ScheduleStore`](src/store.rs) opens `schedules.db`, applies schema
  migrations, and stores schedules and their occurrence history.
- [`Scheduler::start` and `ScheduleExecutor`](src/engine.rs) connect the timer
  loop to the host's executor. `Scheduler::start_with_clock` lets tests provide a
  deterministic clock.
- [`parse_natural_language` and `parse_natural_language_with_tz`](src/nl.rs)
  parse supported English and Turkish phrases into schedule specifications.

## Invariants

The engine reserves an occurrence with its timer update in one transaction and
allows at most one execution per schedule to be in flight. When a schedule is
edited, its revision keeps an older timer decision from applying to the new
definition. Catch-up with `RunAll` is capped at ten occurrences.

After a restart, unresolved occurrences are recorded as missed or interrupted.
Automatic replay would risk starting the same task twice because the executor
does not currently support a durable idempotency key.

## Try a phrase

From the workspace root, run the standalone example to inspect how the
English/Turkish parser interprets phrases **without** starting a daemon or
creating a real schedule:

```sh
cargo run -p scheduler --example parse_schedule -- "every weekday at 8:15am" "in 30 minutes" "not a schedule"
cargo run -p scheduler --example parse_schedule -- --tz Europe/Istanbul "her gün 09:00" "2 saat sonra"
```

Each argument is evaluated separately: matching phrases print their
`ScheduleSpec` (for example `Cron` or `At`), and unsupported phrases print
`no match`. The `--tz` option supplies an IANA timezone name for resulting
cron expressions; relative one-off times are anchored to the current clock.
Run the example with `--help` to see its usage.

## Tests

From the workspace root, run `cargo test -p scheduler --locked`. The tests use
an in-memory database or a temporary data directory. `FakeClock` lets the timer
tests advance time without depending on the system clock.
