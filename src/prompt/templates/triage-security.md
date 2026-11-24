# Task: security surfaces

Walk the supplied diff file by file. Apply these numbered sink surfaces, matching the concept in
whatever language the diff uses (no stack assumptions):

1. **Supply-chain** — install/build hooks (post-install scripts, build-step network fetch), a new
   dependency from an untrusted or unpinned source, lockfile swap to an untrusted registry.
2. **Secrets** — credential/key/token/PEM/connection-string material added in the diff.
3. **CI/CD poisoning** — workflow/pipeline config that runs PR-controlled input with elevated scope
   or exposes long-lived secrets to fork PRs.
4. **Code execution / unsafe deserialization** — shell/process spawn, eval/dynamic import, or a
   deserializer that can execute code, reachable from user-controlled input.
5. **Injection** — user input concatenated into a query/command interpreted by an engine
   (SQL/NoSQL/LDAP/XPath/shell).
6. **Path traversal** — user-controlled path into a filesystem API without normalization.
7. **SSRF** — outbound request whose host or protocol is user-controlled.

For every hit emit exactly one row. `confidence` is an integer 0–10: the probability that the
surface is real **and** reachable from user-controlled input.

`reach` must name the concrete source→sink path. A path is **concrete** only when it cites ALL of:

1. the **source endpoint** — `file:line` where untrusted input enters (request field, CLI arg, file
   content, env, …);
2. the **sink endpoint** — `file:line` where the dangerous operation happens;
3. the **intermediate chain** — how the value travels from source to sink.

If all three are present, `confidence` MUST be 8–10. If any endpoint or the chain is missing or
speculative, `confidence` MUST be 7 or lower — such rows are discarded by the executor. A clean diff
returns `{"rows": []}`.

**Mitigations do not break reachability.** Allowlists, approval cards, rate limits, idempotency keys
and similar mitigations do **not** break the code path. Still report the row and record the mitigation
in `note`; mitigations must not lower `confidence` or change the surface.

Output schema:

```json
{"rows": [{
  "file": "path/as/in/diff",
  "line_start": 12,
  "line_end": 12,
  "quote": "verbatim source line(s), exactly as they appear",
  "surface": 4,
  "confidence": 9,
  "reach": "HTTP body field `cmd` -> exec.Command at line 12",
  "note": "why this is dangerous, in {{OUTPUT_LANG}}"
}]}
```

Rules:

- `file` must be one of the changed files; `line_*` refer to the head checkout, not diff hunk lines.
- `quote` must be a verbatim slice of the supplied diff/source (one or more complete lines).
- Do not emit a tier, severity, summary, or recommendation. Rows only.
