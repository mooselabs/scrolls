# Moving to Scrolls 0.2.0

This release uses store format 3, ontology configuration format 1 and CLI JSON
envelope version 2. Beads backlink payloads remain version 1.

Resolve pending proposals with the previous version or the review commands,
then run `scrolls --actor NAME migrate`. Migration installs the bundled ontology
with **empty startup pins**. It preserves the store UUID (creating one only for
old format 1 stores without one), IDs, revisions, actors, originals, aliases,
source files, edges, operation receipts and Dolt history. Historical proposal
branches remain readable; other writes require migration. No live ledger
migration or documentation-authority cutover is performed.

Export the schema with `scrolls schema export > ontology.yaml`, edit its
instructions and pins, read `scrolls schema show` for the expected schema
revision, and run `scrolls --actor NAME schema apply ontology.yaml --expect REV`.
Inspect the proposal and accept as a human. Incompatible schema edits name
records, fields or relationships; resolve data incompatibility separately.
Automatic bulk rename and transformation migrations are not available.

| Removed interface | Replacement |
|---|---|
| `ask` | `new question` and `link ... asks_about ...`, optionally in a batch. |
| `answer` | Batch `new answer` and `link ... answers ...`. |
| standalone `accept` | Batch the answer's `accepted` and question's `answered` states. |
| `supersede` | Batch status change and a `supersedes` link with reason. |
| `--about`, `--supports` authoring flags | Explicit relationship mutations in a batch. |
| `inbox` | `query --file examples/queries/inbox.yaml`. |
| `doctrine --for PATH` | `query --file examples/queries/applicable.yaml --path PATH`. |
| `stale` | `query --file examples/queries/overdue.yaml --at RFC3339`. |
| `prime --for`, implicit primers/questions/doctrine | Configure pins; use explicit discovery queries for the rest. |

To recover the former primer selection, use `list --kind primer --status current`
and explicitly pin the documents wanted at startup. Use the inbox query for
open personal questions. For previously answered requests, use
`list --kind question --status answered --field "asked_by=$SCROLLS_ACTOR"` and
follow the returned document's immediate answer references.

`review accept` still accepts proposals. Removed document-specific verbs return
migration guidance and perform no compatibility writes. Search includes notes.
Walk defaults to references plus edges; request `--full` for bodies. Context
contains immediate references, including historical and superseded neighbors,
with no ranking or automatic expansion. Follow references explicitly.

`prime --budget` is a soft limit and always delivers guide, instructions and
pins, with actual bytes and overruns. `context --budget` is hard: if the document
alone does not fit, increase the budget to the reported minimum or use `show`.
Budget accounting uses compact JSON, excludes identified protocol metadata, and
includes all content. Existing summary fields are ordinary custom data.
