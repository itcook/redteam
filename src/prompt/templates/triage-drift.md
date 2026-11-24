# Task: convention drift

You are given a `StandardsMap` excerpt for every module touched by the diff. The standard is
whatever the repository actually carries, resolved in this order: explicit doc → machine-enforced
linter config → peer code (sibling files). Treat the pack as the only source of standards.

For each module, read its resolved standard (doc section / linter rule / peer pattern) and the
diff's changes there. Emit one row per deviation:

- `drift: "structural"` — the change crosses a boundary or breaks a pattern the resolved standard
  establishes (placement in the wrong layer/module, naming that breaks the module's contract,
  ownership/API boundary, versioning rule).
- `drift: "local"` — a contained convention slip (formatting, local naming nit, ordering) that does
  not break a boundary.

Name the violated concept in the module's own terms; do not assume a language, framework, or layout.
A module whose `source=no-standard` has no standard to cite: do not invent one — only report a row
there if the diff violates something stated elsewhere in the pack, and say which.

Output schema:

```json
{"rows": [{
  "file": "path/as/in/diff",
  "line_start": 12,
  "line_end": 12,
  "quote": "verbatim source line(s)",
  "drift": "structural",
  "category": "placement",
  "standard": "AGENTS.md#module-layout / rustfmt.toml / peer:src/other.rs:12",
  "note": "what is violated, in {{OUTPUT_LANG}}"
}]}
```

Rules:

- `category` ∈ `placement` | `naming` | `ownership` | `versioning` | `other`.
- `file` must be one of the changed files; `quote` must be verbatim source text.
- No severity, no summary, no recommendation — rows only. A clean module emits no row.
