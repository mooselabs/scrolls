# Verified database backups

`scrolls backup` creates and checks database-only recovery artifacts for shared
format 5, writer generation 3, with Dolt 2.2.0. It is Linux-only and does not
activate a replacement service or change client bindings.

Run creation on the SQL host, with a named account authorized to read the store,
read the private host nonce through SQL, and invoke `DOLT_BACKUP`:

```sh
scrolls --connection ./private-connection.json --dolt .tools/dolt \
  backup create --out /backup/knowledge-new
scrolls --dolt .tools/dolt backup restore /backup/knowledge-new \
  --out /recovery/knowledge-new
```

Both destination parents must already exist; each destination must be new.
Paths must be UTF-8, without parent traversal or symlink ancestors. Creation
requires the SQL process to run under the same local filesystem permissions as
the client: before any server-side backup write it must read an exact fresh
nonce through the client's held directory descriptor. A loopback address or SSH
tunnel alone does not establish this. File URLs are constructed internally;
there is no arbitrary remote backup target or overwrite option.

Creation holds the cooperative database publication lock from source inspection
through native copy, actual disposable restore, `dolt fsck --quiet`, common-engine
graph/provenance validation and source recheck. It compares branch heads, roots,
schema and complete keyed rows, annotated tags and remote references. Clean
pending proposals and durable interrupted-operation intents remain recoverable.
The complete native dataset-root hash is checked after restore. A second
source copy must preserve all named refs and clean working/staged roots; only
working-set timestamps may change because native `DOLT_BACKUP` refreshes them.
Active merges and native rebase branches/plans are refused. Native working-set
display is not a full binary serialization; merge/rebase refusal and full
versioned-table checks are independent of its timestamp normalization. Native
address maps are traversed recursively, including multilevel
maps; each dataset must be a known branch, tag, remote reference, branch working
set or the native creation marker. Other categories (including workspaces,
other internal refs, statistics, tuples and stashes) are refused. Dirty
branches are refused without reset. The native manifest must use version 5
and the `__DOLT__` format.

The artifact is hashed before and after verification. Restore checks every file
path, byte hash and mode, restores into a new private directory, runs fsck and
repeats the same database validation. Symlinks, special files, incomplete
artifacts and changed inventories are refused. A moved backup remains usable.
The manifest is an integrity proof, not a signature from a trusted publisher.

Only `verified.json` marks success. `attempt.json`, a copied snapshot or a native
command exit alone does not. Preserve an interrupted directory for diagnosis;
retry with a fresh destination. A lost acknowledgement can leave a usable copy
without verification, and that copy is deliberately not accepted as a backup.
Native object dumps are captured in anonymous private files and discarded,
so commit descriptions containing authored bodies do not become diagnostic logs.
Hashing streams files; manifests and individual native command outputs are limited to 16 MiB, walks to 100,000 entries and 32 directory levels,
native commands and individual inspection queries to 300 seconds. Native
address-map traversal is bounded to 1,024 nodes and 32 levels. Retained
verification and source-recheck directories require additional disk space.

The JSON response identifies the store, main revision, logical snapshot digest,
proof digest and explicit exclusions. A restore links its source backup proof
and returns the restored `database` directory. Verification starts an embedded
SQL process without a listener; it does not provision accounts or start a server.

SQL accounts/grants, branch permission configuration, server/global and
per-database non-versioned configuration, service activation, client bindings
and the optimizer statistics cache
are excluded. Native inspection may refresh its generated `.dolt/stats` cache;
all other restored files must match their pre-inspection hashes and modes.
A deployable host restore requires a separately tested setup and
account configuration package. Native captured source bytes and modes are
included; native attachment authoring remains a later capability.

## Qualification

`tests/shared_session.rs` uses independent CLI processes and disposable pinned
servers for handbook and research stores. It exercises clean interrupted work
without originating client files, captured assets, tags, remote references,
missing/wrong nonce and nonmatching loopback filesystem, pathname replacement,
corruption, incomplete input, destination collisions, dirty state, lock
contention, source changes and lost backup acknowledgements. Account absence is
asserted before separate disposable provisioning on the restored service.

`scripts/verify-imports.py` also creates and restores the shared PEP corpus
backup after its complete source-byte/mode oracle. These checks qualify the
backup primitive, not full-machine recovery or migration readiness.
