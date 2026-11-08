# Final review: Columbus 0.14.0 rework

*Versions here are as of 2026-11-08.*

## Who runs it and when

One sceptical reviewer in a fresh chat or fresh subagent, after T-1 to T-16 and T-19 to T-25 are done (T-17, the release, waits for this review). You didn't build this, so you see what the builder stopped seeing. Read `docs/build-brief.md`, `docs/build-brief-2.md`, `docs/audit-report.md` and `docs/progress-log.md`, then use the product. You fix nothing. You report, and the owner (who delegated the choice to the builder) decides through the fix set below.

## Run the product

1. Run every acceptance check in `docs/tasks.json` for T-1 to T-16 and T-19 to T-25, and note any that fail now.
2. Run the automated audits:
   - `cargo test --workspace` with the GIS engine installed and `COLUMBUS_TEST_REQUIRE_ENGINE=1`;
   - `pytest workers/python/tests`;
   - `cargo clippy --workspace -- -D warnings`;
   - `python skills/check_skills.py`;
   - a dependency audit (`cargo audit` if available, `pip-audit` on the pinned requirements);
   - a secret scan: grep the rework's diff against `8b6bf173` for token, password, secret and key patterns; only test fixtures may match.
   Install `cargo-audit` and `pip-audit` first if missing.
3. Interfaces: run the app e2e scripts that T-12 extended. A full screen review is out of scope (a separate UI-review project follows).
4. Negative tests:
   - a `files` connection to a folder holding a symlink that leads outside it;
   - an `arcgis` URL on a host not in policy;
   - a trusted environment without `environments.allow_trusted`;
   - a script that opens a network socket in a governed level-A environment;
   - a 0-row CSV, a 1-GB CSV (time it), and a CSV with duplicate header names;
   - the same `export_output` twice;
   - an expired fake ArcGIS refresh token;
   - a web map layer on a host not in `map.hosts`, and a check that no request from the map page carries a token.
5. Confirm each security baseline item marked applied in the brief, especially items 1, 2 and 9: no token in any audit line, provenance file or tool result; the ArcGIS `where` sent is always rebuilt (R-10b).

Record what you ran and each result, because a review that ran nothing is only opinion.

## Lenses

- **Security and privacy:** does any new path let the agent read past a row filter or hidden field, reach a host outside policy, see a token, or register or change an environment?
- **Correctness:** row counts survive paging, caching and exports; CRS and Z/M values survive.
- **Failure handling:** every new failure follows R-14 (cause plus fix, no stack trace).
- **Maintainability:** new code follows the patterns of commit `dbe0f20`; `docs/reference.md` matches behaviour.
- **Performance:** REQ-20 numbers; the 1-GB CSV time.
- **Intent and success measures:** each of the seven success measures (part 1's six, part 2's seventh) is met; walk the owner's ROW workflow (part 2, "The job") yourself on the fixtures.
- **Upkeep:** could the owner follow the Keeping it running runbook next month, and does every change stream have an owner?

## Scoring

This scale is this review's own; the audit report used the audit's labels, where wrong results reported as complete are Fix now whatever their score.

Only findings with a concrete harm scenario count: who is hurt, how, and what triggers it. Score severity and likelihood from 1 to 10 each; priority is severity × likelihood.

| Priority | Label |
|---|---|
| 40 or more, or severity 9+ with likelihood 3+ | Fix now |
| 20 to 39 | Fix when convenient |
| Under 20 | Accept |

Severity guide: 10 means data lost or leaked; 7 means a main flow broken; 4 means a workaround is needed; 1 is cosmetic. Likelihood guide: 10 means every user, every time; 5 means some users, often; 1 means rare and unusual.

## What the person sees

- Three lines: the verdict, the worst risk, and what you recommend.
- One table:

| # | Finding | Harm scenario | Severity | Likelihood | Priority | Label | Suggested fix |
|---|---|---|---|---|---|---|---|

- The recommended fix set: every Fix-now finding, plus any cheap Fix-when-convenient ones. The owner delegated approval, so the builder adopts every Fix-now finding and logs the rest.

## After the review

The fixes chosen become fix tasks numbered from `R-101` in `docs/tasks.json`, done like any task, with evidence. Then one targeted re-check, in a fresh context and of those fixes only, confirms each one and that nothing next to it broke. Then T-17.
