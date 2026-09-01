## Review approach: decompose

Do not review this change in a single pass. A large diff skimmed once hides
subtle bugs; reviewing it in focused pieces surfaces them. Work through it in
two phases, then report findings in the exact format the instructions above
already specify.

1. **Chunk the changed files into <= 4 coherent groups** by what the code
   _does_, not by path prefix — a feature spanning several directories belongs
   in one chunk. Put each sensitive surface (auth/authz, money movement,
   schema/migration, secrets/external integrations) in its own chunk so it gets
   the closest read. A small change may only have one or two real chunks; do not
   pad to four.

2. **Review each chunk closely in turn** — keep the whole diff in mind for
   context, but concentrate your scrutiny on that chunk's files. Then do one
   dedicated **seam pass** over what crosses chunk boundaries: changed exported
   signatures, shared types, migration-vs-code column drift, API
   request/response shapes — the bugs a file-by-file read structurally misses.

This approach changes only _how thoroughly you look_. It does not change the
output format, the severity rubric, or the `Goal:` / `Approach:` / `Purpose:` /
`Proof:` tags — emit
findings exactly as instructed above, one per real issue, each with a
`file:line` and a `Fix:`.
