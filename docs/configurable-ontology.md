# Configurable ontology and priming

Status: product direction agreed 2026-10-16; implementation design proposed.
The current implementation still uses fixed kinds and query rules. This document
sets the target and acceptance criteria; its example syntax is not a shipped API.

## Direction

Scrolls should support user-defined information architectures and ontologies.
The ontology defines what records mean; retrieval profiles define which records
an agent needs and how to present them. Both are versioned configuration.

The existing knowledge ontology becomes a bundled default configuration. Its
decisions, evidence, questions, doctrine, entities, statuses, and relationships
must not be compulsory concepts in the engine. A simple handbook can use one
document kind and one priming rule. A different ontology can use different kinds,
fields, relationships, lifecycles, and priming instructions without a Rust change.

The first priming language must cover our existing use cases and other simple
ones. General ontology support does not require an unrestricted programming
language or a universal inference system. Extend the declarative vocabulary only
when a concrete use case cannot be represented clearly.

## Configuration boundaries

| Configuration | Responsibility |
| --- | --- |
| Ontology | Kinds, descriptions, fields and types, required values, defaults, lifecycles and allowed transitions, relationship endpoint constraints, required reasons, cardinality and cycle constraints, and review/write policies. |
| Retrieval profiles | Named `prime`, `context`, and external-work context profiles; selection, exclusions, traversals, ordering, output blocks, and representation. |
| Agent instructions | Explain the ontology and intended workflows, with command examples. Instructions may accompany a profile; prose does not replace validation or mechanical retrieval rules. |
| Import/export mappings | Map source document types, fields, paths and relationships to the configured ontology. Liskov mappings belong to its adapter/profile. |
| Engine | Stable identities, revisions, atomic writes, optimistic concurrency, operation receipts, review mechanics, deterministic evaluation, bounded traversal, byte accounting, diagnostics, and consumption provenance. |

The minimal record envelope remains an engine contract. Domain fields such as
confidence and review cadence need not be compulsory for every ontology.
Human/agent write policy remains cooperative local policy until authenticated
identities are implemented.

Configuration must be validated against one versioned format and activated
atomically with the store state it governs. Recommend storing the active ontology
and profiles in Dolt, with YAML/JSON import/export for authoring and distribution.
A local file and a database registry must not become competing runtime authorities.
Changing a field, lifecycle, or relation must either validate existing data or
carry an explicit migration. Configuration changes participate in review and
history; packets identify the configuration revision used.

Default convenience commands such as `ask` and `supersede` must resolve their
meaning through declared schema operations, or report that the selected ontology
does not support them. Generic record and relationship commands remain usable.
External adapters own protocol and identity handling; profiles own which linked
records count as relevant, current, historical, or reference-only.

## A small retrieval language

Use named profiles over the same evaluation engine. A profile contains ordered
selection/traversal stages and output blocks. The initial primitives are:

- Boolean predicates (`all`, `any`, `not`), equality/membership, field existence,
  and explicit missing-value semantics. Values can reference actor, root, paths,
  and an external work reference supplied by the invocation.
- Sources: all records, the root, an earlier named result set, and records linked
  to a qualified external reference. Selection operates on the chosen branch.
- Directed traversal (incoming, outgoing, either), relation and endpoint filters,
  relationship existence/absence checks, and a bounded depth.
- Path-glob matching over configured fields or normalized imported triggers.
  Conditions requiring human judgement produce an unresolved diagnostic rather
  than silently evaluating to false.
- Ordered stages and reusable named selections, including acyclic composition
  needed to apply entity context to several linked documents. References to later
  stages, recursive profiles, and unknown names fail configuration validation.
- Named output blocks with full-record or reference projections, explicit
  ordering/priority, stable tie-breaking, exclusions, and duplicate handling.

For a simple handbook, the intended authoring experience is approximately:

```yaml
profiles:
  prime:
    instructions: "Read the current handbook pages before making changes."
    blocks:
      handbook:
        select:
          kind: page
          status: current
        format: full
        order_by: id
```

This is an illustrative shorthand, pending the actual configuration schema. It
must not require users to understand stages or graph traversal when they only
need a filtered list. More complex profiles can add stages and relationships.

The packet assembler has a documented deterministic contract: block order,
stable ordering within blocks, projection, duplicate handling, and byte-budget
packing. Preserve the current skip-and-continue behavior unless a versioned
profile explicitly changes it. Report omitted IDs and unresolved conditions.
Define whether instructions and block wrappers count toward the content budget;
metadata outside that budget must be identified. Never label byte counts tokens.
Provide an explanation of why each item was selected, excluded, or omitted so
profile authors can debug rules without guessing.

## Coverage required before replacing the existing rules

Baseline source: `6d132e6` (`src/app.rs`, `src/query.rs`, `src/model.rs`,
`src/policy.rs`, `src/import.rs`, and the Beads adapter). Refresh this inventory
against the implementation head before coding; concurrent changes may add rules.

| Existing behavior | Required representation |
| --- | --- |
| Prime includes active path-matched doctrine from imported routes, glob edges and record fields; prose scope clauses are reported as undecided. | Normalized routing inputs, glob predicates, active-state filters, diagnostic outputs. |
| Prime includes all current primers, plus non-frozen questions that are open and owned by the actor or answered and asked by the actor. | Boolean composition, actor variables, nested fields, explicit global selection. |
| Context includes its root; accepted unsuperseded decisions and current knowledge/primers via incoming `about`, open questions via `asks_about`, and active doctrine via `applies_to`. | Root source, directed traversal, typed/status predicates, absence of incoming supersession. |
| Context includes related entities in either direction and supporting evidence for the entire selected full-record set as references. | Endpoint filtering, staged traversal, reference projection. |
| Bead context expands linked documents and their entities, includes accepted answers for linked answered questions, ranks open questions before accepted decisions, current knowledge, active doctrine and accepted answers, and summarizes other candidates. | External-reference source, reusable context selections, conditional traversal, rank rules and a fallback reference block. |
| Bead context includes evidence, removes full/reference duplicates, and returns linked-document metadata and a scope limitation. | Set operations, explicit duplicate policy, profile metadata and diagnostics. |
| Packets omit notes, order deterministically, skip oversized records and continue, and report omissions. Prime puts doctrine first; ordinary context puts full records before references. | Configured exclusions and ordering; shared packing contract. |

Existing ordinary context can emit the same record in full and as a reference;
Bead context removes that duplication. Preserve these differences in compatibility
fixtures or approve and document the behavior change explicitly. Likewise,
path-scoped primers would be a change from today's global primer selection.

Ontology coverage must also inventory every kind/status/relationship branch in
validation, mutations, import/export and external adapters. Replacing query
switches alone does not make Scrolls ontology-configurable.

## Acceptance and implementation sequence

1. Freeze representative fixtures for existing prime, context, doctrine and Bead
   context behavior, including ordering, exclusions, unresolved clauses, duplicate
   handling, small budgets, archived/superseded records and selected branches.
2. Define and validate the versioned ontology/profile format. Make the runtime
   registry authoritative for schema validation and policies, and migrate existing
   stores to the bundled default without losing records or review history.
3. Implement the bounded evaluator and packet assembler. Run the default profiles
   against the fixtures and require equivalent output or an explicitly documented
   change. Verify revision and consumption provenance alongside content.
4. Prove independence with a small handbook ontology and a second unrelated
   ontology, such as research papers, claims and citations. Neither may need
   knowledge-specific kind names or a Rust change. Demonstrate configuration of
   authoring, validation, priming and context, not just successful parsing.
5. Document the default and simple profiles, migration, validation errors and
   rule explanations. Reject malformed or unsupported configuration explicitly;
   preserve offline operation and prohibit implicit network/model execution.

This direction does not claim that the evaluator, editable runtime registry,
custom ontology migrations or schema-aware priming instructions already exist.
