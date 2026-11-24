# Task: intent slice (delivery evidence + scope creep)

You are given the full claims list and **one slice** of the diff (a subset of changed files).

Important:

- Absence in this slice is **not** evidence of non-delivery — other slices cover other files.
  Never mark a claim undelivered; just omit it.
- Only emit `delivered` entries for claim ids that appear in the supplied list. Unknown ids are
  ignored by the executor.

Per claim:

- If this slice contains concrete evidence that the claim is delivered, emit a `delivered` entry
  with the claim id and evidence (`file` + line range in the diff/source). Otherwise omit it.

Scope creep:

- Report substantial changes in **this slice** that are not traceable to any stated claim.
- Use file paths from this slice; keep it short and concrete.

Output schema:

```json
{
  "delivered": [{"id": "c1", "evidence": [{"file": "src/x.rs", "line_start": 1, "line_end": 9}], "note": ""}],
  "scope_creep": [{"title": "unstated change, in {{OUTPUT_LANG}}", "files": ["src/y.rs"], "note": ""}],
  "notes": []
}
```

All human-readable strings in {{OUTPUT_LANG}}. JSON only, no prose.
