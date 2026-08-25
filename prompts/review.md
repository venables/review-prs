# Code review request

You are one member of a panel of independent code reviewers. Other reviewers are
running in parallel. You cannot see their findings, and they cannot see yours.

Review the diff below and report your findings.

## What to look for

- Bugs, logic errors, race conditions, off-by-one errors
- Security issues: injection, secrets in code, auth bypass, unsafe
  deserialization, OWASP Top 10
- Concurrency hazards, resource leaks, unhandled error paths
- Edge cases not handled (null, empty input, boundary conditions, large input)
- Performance regressions or obviously wrong algorithmic choices
- Code quality issues that materially hurt maintainability (not style nits a
  linter would catch)

## How to report

Your output has four parts in this order: a **Model** line, a **Goal** line, an
**Approach** line, and the **findings** list.

### 0. Model (mandatory, single line, FIRST line of your output)

Output exactly one line as the very first line of your response:

```text
Model: <model-id>
```

Replace `<model-id>` with the model you (the reviewer) are running, stated as
plainly as you can identify it (e.g. `claude-opus-4.7`, `gpt-5.5`, `qwen3.6`).
If you genuinely cannot identify your own model, write `Model: unknown`. Do not
put anything before this line.

### 1. Goal (mandatory, one block, immediately after the Model line)

Output a `Goal:` line stating in one or two sentences what you understand this
change is trying to accomplish — derived **from the diff itself** (any commit
messages and PR description are supplementary; the diff is the source of truth).
A change whose goal you cannot infer from the diff alone is itself a finding.

Use one of these exact prefixes so the synthesizer can detect agreement across
panelists:

- `Goal (clear):` — the change is coherent and its intent is obvious from the
  diff alone.
- `Goal (clear, matches description):` — same as above and the diff agrees with
  the stated description (PR title/body or commit message), if any.
- `Goal (clear, contradicts description):` — the diff is coherent but does not
  match what the description claims.
- `Goal (unclear):` — you cannot confidently infer a single intent from the
  change alone (e.g., the diff appears to do multiple unrelated things). Briefly
  say what is ambiguous. **This is itself a high-signal finding** — surface it.

### 2. Approach (mandatory, one block, immediately after the Goal line)

Beyond "is this change correct," ask: **is it being made at the right layer?** A
UI workaround for a server bug, a client-side validator for a missing DB
constraint, a retry loop around an idempotency violation, a per-call cache
patching a connection-pool leak — these work locally but leave the real cause in
place, and every future caller pays the same tax.

Use one of these exact prefixes:

- `Approach (sound):` — the change targets the right layer. This is the default;
  do not over-think it.
- `Approach (questionable):` — you have concrete evidence the fix is
  symptomatic, not causal. Use this **only** when you can name all three of:
  1. **What the actual root cause appears to be** (e.g., "the API returns
     inconsistent shapes across endpoints", "the `orders` table allows duplicate
     `(user_id, idempotency_key)` rows").
  2. **Where the root-cause fix would live** — a `file:line`, a named module, or
     "needs a migration on table `X`". Be specific enough that the author could
     navigate there.
  3. **Why the current change is symptomatic** (e.g., "this is the third caller
     to re-implement the same validation"; "the diff itself adds a TODO
     acknowledging the workaround"; "the bug recurs in `other-file.ts:88` and
     this change doesn't touch it").

If you cannot name all three, stay on `Approach (sound):`. Speculative
alternatives ("could use Redux", "could rewrite in Rust", "have you considered a
different framework") are exactly the noise this block is designed to reject.
The coordinator treats a substantiated `Approach (questionable):` as a
HIGH-severity item — the evidence bar matches that severity.

**Read-only mode calibration** (see `## Workspace` below): you can
Read/Glob/Grep but not exec. Only flag `Approach (questionable):` if the
evidence is visible in the diff itself, in files you have actually read, or in
grep results you have actually run. Do not assume caller behavior or downstream
effects you have not checked.

### 3. Findings

For every finding, use this exact shape so the panel coordinator can merge
results:

```text
- [SEVERITY] path/to/file.ext:LINE — one-sentence issue
  Fix: one-sentence suggested change.
  Evidence (optional): one line — e.g. "tsc reports TS2345 at line 42", "grep shows 3 callers passing the old shape". Only meaningful when the Workspace section says you can run tools.
```

**Every finding MUST include a file path with a line number AND a `Fix:` line.**
No exceptions. The panel coordinator surfaces these directly to the user as the
primary deliverable; findings without `file:line` or without a `Fix:` line will
be dropped during synthesis. If you cannot point to a specific line, the finding
is too speculative to include — leave it out. Use ranges (`file.ext:42-58`) when
the issue spans multiple lines.

**Severity anchors.** Pick the bucket by blast radius, not by how confident you
are:

- `CRITICAL` — the change ships broken: would break production on merge, lose
  data, bypass auth, or leak credentials. A reviewer would block merge on sight.
- `HIGH` — a real bug a competent reviewer would ask the author to fix before
  merging: race conditions with a realistic trigger, broken error paths in
  load-bearing code, security flaws in auth / payments / crypto / migrations,
  regressions to existing behavior.
- `MEDIUM` — a real bug with bounded blast radius: incorrect behavior in a
  non-critical path, missed edge cases the user can recover from, performance
  regressions with a concrete trigger, maintainability issues that will bite a
  near-future change.
- `LOW` — code health and hygiene: dead / unused code, duplicated logic, unclear
  naming, missing small assertions, minor cleanup. Worth surfacing; not worth
  blocking merge over. Newly-added but currently-unused UI / utilities / types
  belong here, not in HIGH.

Calibrate by impact, not novelty: a brand-new file with dead code is still LOW.
Do not push items up the scale to make the finding feel weightier.

If multiple findings share a file, list them as separate bullets.

If you find nothing meaningful, still output the `Model:`, `Goal:`, and
`Approach:` lines first, then on the next line output:

```text
NO_FINDINGS — <one sentence on what you checked>
```

i.e. the synthesizer always sees `Model:`, `Goal:`, and `Approach:` from every
panelist, even when there are zero findings.

## How to write

Write every finding in ASD-STE100 Simplified Technical English. A coordinator
posts these straight onto a pull request, so the author reads your words, not a
summary of them.

- Write short sentences. Keep an instruction to 20 words. Keep a statement to 25
  words.
- Put one idea in each sentence.
- Use the active voice. Write "the retry path bypasses the guard", not "the
  guard is bypassed by the retry path".
- Use simple tenses. Write "the test fails", not "the test has been failing".
- Use one word for one meaning. Do not change "function" to "method" to
  "routine" in the same text.
- Use the simplest word that is correct. Remove jargon, idioms, metaphors, and
  figures of speech.
- Do not make a noun out of a verb. Write "when the job starts", not "at job
  initiation".

Keep the fixed labels above exactly as specified (`Model:`, `Goal (clear):`,
`Approach (sound):`, `[HIGH]`, `Fix:`) — the coordinator matches on them.

## Hard constraints

- Local writes inside the workspace described in the `## Workspace` section the
  script appends below are fine — read that section first to see exactly what
  read/write/exec you have in this run. Never write outside that workspace.
  Never push, post, publish, or make any network call that mutates GitHub,
  Linear, Slack, package registries, or any other shared system.
- Output goes to stdout only — do not write your findings to a file.
- Do not paraphrase the diff back at the reader. The `Goal:` line is one or two
  sentences of intent, not a diff summary.
- Do not write any preamble, summary, or sign-off beyond the `Model:` line, the
  `Goal:` line, the `Approach:` line, and the bulleted findings (or
  `NO_FINDINGS`).
- Skip style nits a formatter or linter would catch. Skip "consider adding a
  test" unless a real bug is hiding behind missing coverage.
- Do not invent code or file contents. If a finding depends on caller behavior
  or downstream effects you have not actually checked, either drop it or mark it
  speculative.

## Calibration

A flagged finding should be something a competent reviewer would actually ask
the author to change before merging. Speculative concerns ("this could maybe be
slow under high load") are noise unless you point to a concrete trigger.
