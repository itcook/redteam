# Task: intent vs diff

You are given the PR body and its linked issues (the author's stated intent) plus the full diff.
Enumerate the PR's **stated intent** as checkable claims — things the AUTHOR asserted they would do.
Then decide whether the diff delivers each claim.

Rules for claims:

- Only `body` and linked issues (`issue#N`) are claim sources. The PR title is a summary, not a
  claim. Comments are context, never claims. CI status, pre-existing failures and lint output are
  observations, never claims.
- A claim must be checkable against the diff, not a generic aspiration.
- `delivered: true` only with concrete evidence (file + line range in the diff). If the evidence is
  absent, `delivered` is `false` and the claim is undelivered.

Scope creep: substantial diff changes not traceable to any stated claim.

Maintainer overlap: if a non-bot reviewer (not the PR author) explicitly says this duplicates or is
already covered by existing functionality, set `maintainer_overlap.flagged=true`, copy that reviewer's
login into `author`, and quote their exact words in `quote`. Otherwise leave it false/empty. Never
infer redundancy without such a comment.

Output schema:

```json
{
  "claims": [{
    "claim": "stated assertion, in {{OUTPUT_LANG}}",
    "source": "body",
    "delivered": false,
    "evidence": [{"file": "src/x.rs", "line_start": 10, "line_end": 14}],
    "note": "why it is or is not delivered"
  }],
  "scope_creep": [{
    "title": "unstated substantial change",
    "files": ["src/y.rs"],
    "note": "why it is not traceable to a claim"
  }],
  "maintainer_overlap": {"flagged": false, "author": "", "quote": ""},
  "notes": ["observations that are not claims"]
}
```

`source` must be `body` or `issue#N`. Never use the title or comments as a source. A PR with no
stated intent returns empty `claims` and only genuine scope-creep items.
