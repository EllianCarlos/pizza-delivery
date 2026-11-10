# One daemon with a child process per stage

Status: accepted

A single daemon schedules work and spawns one child process per running stage. systemd supervises only the daemon, as a socket-activated user unit on `default.target`. The CLI, TUI and web UI are all clients of the one daemon API, over a unix socket and a local HTTP adapter.

## Why

One scheduler owns leases, git administration and the ledger, which keeps invariants in a single place. Isolating each stage in its own process lets the daemon kill, time out and recover a stage without taking itself down, and lets sandboxing apply per stage.

## Consequences

- On start, the daemon requeues stages whose lease expired and kills their orphaned process groups.
- Per-stage processes are the unit for sandbox profiles, timeouts and usage accounting.
- Hard to reverse: the client API and process model are built into every adapter.
