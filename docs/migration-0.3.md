# Scrolls 0.3 migration

0.3.0 uses store format 4, ontology/profile/plan format 1 and JSON envelope 2.
Resolve pending proposals, then run `scrolls --actor NAME migrate` from main.
`--dry-run` reports the migration without changing the graph or writing a receipt.
Existing records, aliases, UUID, receipts, source files and historical branches
are preserved. Format-3 custom ontologies remain configured as before.
Older writers reject format 4. Historical branches remain readable.

Migration creates the three import collections on a journaled temporary Dolt
branch. DDL is not transactional in Dolt 2.2.0. The engine validates the complete
state before advancing main by fast-forward; recovery never resets main. Keep
`migration-journal.json` when an unexpected-state error requests inspection.
Ordinary opening/doctor completes verified recovery; planning refuses to recover.

Use `import markdown` for existing Liskov snapshots. Their IDs and legacy
`source_files` table are unchanged. Generic sources use `import plan git` and
`import apply`; they require native export. Sources are independent namespaces.
Accepted sources cannot be refreshed to different commits or profiles in 0.3.
Local interpretations belong in local records, linked to captured source records.
