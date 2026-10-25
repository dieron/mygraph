# Operation: `retract`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.6](../../../docs/06-agent-api.md)

Soft-deletes or restores objects. Lifecycle and cascades: [model/objects.md §8](../../model/objects.md#8-lifecycle).

## 1. Request

```json
{ "graph": "research", "action": "retract", "targets": ["l_9x", "c_19"], "reason": "satire site" }
{ "graph": "research", "action": "retract", "batch": "b_8k3", "reason": "wrong article" }
{ "graph": "research", "action": "restore", "targets": ["n_3kq"] }
```

**[OP-RETRACT-1]** `action` is `retract` (default) or `restore`. Exactly one of `targets` (1–1000
object IDs or keyed references `{"kind": "node", "key": "…"}`) or `batch` (a batch ID) is required;
`reason` is optional text ≤ 1024 bytes stored in the change log.

**[OP-RETRACT-2]** With `batch` and `action: retract`, the targets are all objects **created** by
that batch that are still active. With `batch` and `action: restore`, the targets are all objects
**retracted** by that batch (directly, not by cascade) that are still retracted.

**[OP-RETRACT-7]** When restoring several targets, they are processed in the order nodes, links,
evidence, citations (each group in request or batch order), so that endpoints and citation
targets are active before the objects that depend on them are restored.

**[OP-RETRACT-3]** Targets already in the requested state are reported as `unchanged`, not as errors.
A merged node as target fails with `lifecycle.invalid_transition`.

**[OP-RETRACT-4]** Cascades follow [OBJ-LIFE-2] and [OBJ-LIFE-3]. Restoring a link or citation
directly requires its endpoints / evidence and target to be active
(`link.endpoint_not_active`, `citation.evidence_not_active`, `citation.target_not_active`) and its
identity to be free ([STORE-LINK-1], [OBJ-CIT-1]; reason code `lifecycle.identity_taken`).

**[OP-RETRACT-5]** Restoring a node or link re-applies the evidence policy ([OBJ-POLICY-1]).

## 2. Response

```json
{
  "batch": "b_8k4",
  "items": [ { "id": "n_3kq", "outcome": "retracted" },
             { "id": "l_9x", "outcome": "retracted", "cascade_of": "n_3kq" } ],
  "warnings": [ { "code": "UNSUPPORTED_FACT", "details": { "id": "n_51a" } } ]
}
```

**[OP-RETRACT-6]** `outcome` is `retracted`, `restored` or `unchanged`; cascaded items carry
`cascade_of` with the ID that caused them. Warnings: `UNSUPPORTED_FACT` ([OBJ-POLICY-3]),
`NOT_RESTORED` ([OBJ-LIFE-2]).
