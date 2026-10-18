# Shared SQL session foundation

This is a development foundation, not a shared-authoring release. The CLI still
opens the local `Store`. Shared proposal publication, durable recovery journals,
writer/schema fencing and the full authoring integration remain prerequisites
before a shared store can become authoritative.

The foundation in `src/storage/session.rs` provides one `SqlSession` boundary
for the existing local store and a MySQL-protocol `SharedSession`. It uses one
owned connection for the entire operation, without pooling, reconnection or
write retry. Credentials are supplied by the caller and are neither serialized
nor included in a debug representation. The initial transport uses TCP without
TLS and requires a trusted encrypted network; qualification uses loopback only.
It must not be exposed to an untrusted network.

## Contract

- Require exactly Dolt **2.2.0**, the existing private binary pin.
- Acquire a database-wide named lock before selecting or reading the candidate
  branch. The key excludes the account and branch, folds database-name case,
  and hashes the namespace to fit MySQL's 64-byte lock-name limit.
- Require a numeric endpoint address; synchronous DNS is outside this transport
  and is not bounded by a TCP timeout. Bound connection, IO and lock acquisition.
  Start a fresh transaction snapshot
  after acquiring the lock. Disconnect releases the session lock.
- Explicitly select `database/branch` on that connection. Reject unsafe names.
- Refuse unexplained dirty state on any branch; never discard it on open.
- Commit already-validated rows and their operation receipt in one transaction
  with a Dolt commit. Verify the returned hash against HEAD before reporting
  success. The caller owns graph validation and operation input hashing.
- On a failed commit, roll back SQL if possible and require receipt
  reconciliation. SQL rollback does not prove a Dolt commit never happened.
  Loss of the commit response is reported as indeterminate, never retried.

This is cooperative serialization. Raw SQL clients can bypass both the lock and
policy. This module does not implement database administration, migrations,
backup, account provisioning, operation replay or graph validation. Those are
not implied by the transport's receipt persistence tests.

## Executable qualification

```sh
scripts/install-dolt.sh
cargo test --locked --test shared_session
```

The test suite starts disposable private servers on ephemeral loopback ports.
It fails if the pinned binary is absent, creates a named fixture account and
removes only its own temporary data. It proves:

1. Two sessions select different branches without switching each other.
2. A reader retains its repeatable-read snapshot until transaction end.
3. A contender on another branch times out, including a positive bounded wait.
4. Disconnect and an independently killed client release the publication lock.
5. Killing a client abandons its uncommitted rows.
6. A waiting writer reads the new base after the previous publisher releases
   the lock.
7. Invalid multi-statement changes roll back; successful rows and receipts
   survive a hard server stop and restart.
8. A duplicate receipt primary key cannot overwrite the prior receipt.
9. A protocol proxy discards the server's Dolt-commit acknowledgement. The
   writer reports an indeterminate outcome; another client recovers both the
   committed row and the original receipt without a local journal.
10. Raw SQL dirty state on an inactive branch is refused and preserved.
11. SQL numeric, null and text types match the local transport, and invalid
    database and branch names fail before SQL execution.

The `client_process` test is a subprocess entrypoint used by the kill test;
it returns immediately when invoked without its parent-provided fixture values.
No fixture contacts Beads, a factory or a model service.

The upstream branch/session semantics are documented in
[Dolt's branch reference](https://www.dolthub.com/docs/sql-reference/version-control/branches/).
The tests pin behavior to 2.2.0 rather than assuming the latest documentation
qualifies that version.

## Remaining storage qualification

Wire the shared session into the existing authoring, import and review engine;
validate complete candidate graphs while holding the lock. Move review recovery
from client files into durable database state, accept on operation-specific
branches, and verify main advancement. Then qualify stale revisions, simultaneous
graph constraints, operation-ID replay and changed inputs, proposal conflicts,
stale human review tokens, recovery from a different client and unsupported
writers. Schema migrations require exclusive maintenance and verified backups.
Passing this foundation does not satisfy those remaining tests or migration
readiness.
