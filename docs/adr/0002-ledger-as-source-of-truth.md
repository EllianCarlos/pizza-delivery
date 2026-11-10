# Ledger as source of truth

Status: accepted

The append-only events table is the source of truth. Job and stage state derive by folding events through the pure state machine. The `jobs` and `stages` tables are rebuildable projections. Append-only is enforced in the database with triggers rejecting `UPDATE` and `DELETE` on the events table, and events are kept forever.

## Why

Every question about what happened to a job (audit, crash recovery, budgets from history, live tailing) is answered from one immutable record. Projections can be dropped and rebuilt, so schema changes to views never lose data.

## Consequences

- All state changes are expressed as events; there is no side-channel mutation of job state.
- Operational data that is not history (leases, push subscriptions) lives in separate mutable tables and is rebuildable.
- Hard to reverse: the event envelope and families become a long-lived compatibility surface.
