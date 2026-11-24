# Task: stated intent (claims + maintainer overlap)

You are given the PR body, its linked issues, and reviewer comments (non-bot, non-author).
Do **not** read a diff in this phase; only enumerate what the author said they would do.

Claims:

- Only `body` and linked issues (`issue#N`) are claim sources. The PR title is a summary, not a
  claim. Comments are never claims.
- Each claim must be a checkable assertion the author made. CI status, pre-existing failures and
  lint output are observations, not claims.
- Do not assign ids; the executor assigns `c1..cn` in the order you emit them.
- If there is no stated intent, return an empty `claims` array.

Maintainer overlap:

- Set `flagged=true` only when a non-bot reviewer (not the PR author) explicitly says this
  duplicates or is already covered by existing functionality.
- Copy that reviewer's login into `author` and their exact words into `quote`.
- Otherwise leave `flagged=false` with empty fields. Never infer redundancy without such a comment.

Output schema:

```json
{
  "claims": [{"claim": "checkable assertion, in {{OUTPUT_LANG}}", "source": "body|issue#123", "note": ""}],
  "maintainer_overlap": {"flagged": false, "author": "", "quote": ""},
  "notes": ["observations that are not claims"]
}
```

All human-readable strings in {{OUTPUT_LANG}}. JSON only, no prose.
