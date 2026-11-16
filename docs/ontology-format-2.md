# Upgrade an ontology to format 2

Ontology format 2 calls the generic external work target `work`. Standalone
Scrolls uses Beads references for it: `beads://PROJECT-UUID/FULL-ISSUE-ID` stays the
canonical stored form. Existing edge targets remain byte-identical. Format 1
continues to use `beads` in relation `external` lists and `body_links`; it remains
readable and writable, including historical revisions and proposals.

Loading, reading and ordinary writes never convert a stored ontology or change
its revision. A format-1 revision continues to identify the same schema in import
sources, Markdown plans, backup manifests and `--expect` values. Only explicit
initialization, migration from a store format below 3, or reviewed `schema apply`
writes the ontology. New stores and migrations from store formats below 3 get
the bundled format-2 ontology. Store formats remain 4 local and 5 shared; import
profiles and Markdown plans keep their own `format: 1`.

## Reviewed upgrade

Export, edit, apply with the exported revision, then have a human accept:

1. Run `scrolls --json schema show` and retain `data.schema_revision` as `REV`.
   Run `scrolls schema export > ontology.yaml` to export that active ontology.
2. Edit `ontology.yaml`: set `format: 2`, replace `beads` with `work` in relation
   `external` lists, and rename the `body_links.beads` key to `body_links.work`.
   Keep the relation names, other rules and all `beads://` references unchanged.
3. Run `scrolls --actor alice schema apply ontology.yaml --expect REV` with the
   actual revision from step 1. This creates a proposal; main still uses format 1.
4. Inspect `scrolls --json review BRANCH`, retaining `data.review_token`. A human
   accepts with `scrolls --actor alice --role human review accept BRANCH --review-token TOKEN`.
   Substitute the returned branch and token. If either proposal or main changes,
   inspect review again and use its new token.
5. Run `scrolls schema show` and `scrolls lint` to verify the accepted configuration.

For a store using the unchanged format-1 bundled ontology, applying
`schemas/knowledge.yaml` from this release in step 3 yields the same revision as
a fresh store's bundled ontology. The Liskov and Markdown importers require the
bundled ontology, now format 2: apply it first to a store on the format-1 bundled
ontology. Existing import sources and plans keep their recorded schema hash;
regenerate plans as needed against the newly applied revision before importing.

Older proposals retain their format-1 ontology and can still be read. Acceptance
always validates the merged configuration and graph against current main.

## Writers older than format 2

Older writers are not fenced out of a format-2 store. They deserialize its
ontology, but do not recognize `work`: new work links are refused and body
`beads://` links stop being extracted. Use older writers only to read a format-2
store; use a format-2-aware writer for every write. No store-format or shared
writer-generation bump enforces this instruction.
