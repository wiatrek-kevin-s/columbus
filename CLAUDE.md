# Project instructions: Columbus 0.14 rework

You are building the Columbus 0.14 data-reach rework for the repository's owner, a GIS analyst who does not review code, from an agreed plan. Carry it out faithfully, verify every step, and don't redesign it, because its decisions have reasons you may not see.

**This is a set-and-forget build.** The owner answers once, at the start, then expects you to finish everything without them. If you can do something yourself, do it: merging, starting workflows, writing fixtures, choosing between options the brief leaves open. Ask only for what you truly can't do or decide.

## Intent

> Columbus lets GIS analysts and developers use AI on all of their organization's data, safely. They connect any source (Oracle and SQL Server geodatabases, file geodatabases and shapefiles, ArcGIS Online/Portal, CSV/Excel, web APIs) and set its rules first: read-only or read-write, edit a copy, which rows and fields. The AI then works the data with any Python the analyst has, including ArcGIS Pro's arcpy and environments they build. It follows the organization's SOPs from its documentation, checks its own results, and saves outputs locally. Repeated work becomes workflows and skills. It is fast, dependable, and quick to use.

When the brief can be read two ways, take the reading that serves this intent and log why. No existing rule loosens except where the brief says so (D-3).

## Sources of truth

1. `docs/build-brief.md` (part 1) and `docs/build-brief-2.md` (part 2, the owner's weekly workflow): what to build and why. Follow their IDs (D-n, REQ-n, F-n, R-n, T-n).
2. `docs/tasks.json`: the tasks. Change only `status` and `evidence`; the one exception is adding fix tasks `R-101`, `R-102`, … after the final review.
3. `docs/progress-log.md`: what has happened, newest handoff note first.
4. `docs/reference.md`: how Columbus behaves; keep it true.
5. `docs/audit-report.md` (evidence, AF-n) and `docs/final-review.md`.

Nothing else is a source.

| Area | Tasks | Milestone |
|---|---|---|
| S1 | T-1, T-2, T-3 | Setup, baseline, `files` |
| S2 | T-4, T-5, T-19 | Exports, pager, M values |
| S3 | T-6, T-7 | Environments |
| S4 | T-8, T-9 | Trusted, arcpy, tools, workflows |
| S5 | T-10, T-11, T-12, T-20 | ArcGIS, sign-in, app, workflow reach |
| S6 | T-13, T-14, T-15, T-21, T-22, T-24, T-25 | Speed, pinning, run review, map, web layers |
| S7 | T-23, T-16 | End to end, docs, version |
| S8 | T-17 | Release |

## Start of every session

1. **First session only, the setup round (T-1).** Check every access you need: pushing a branch, reading Actions runs, dispatching workflows, merging to `main` under its branch rules (if a required review blocks self-merge, ask the owner to allow a bypass or drop that rule for this build), and Dependabot alerts. Send the owner **one** message listing everything missing, with click-by-click steps, and wait for that one reply. This is the only planned question.
2. Read the newest handoff note and `docs/tasks.json`.
3. Run the smoke check: `cargo build --workspace` and `cargo test -p columbus-mcp --test e2e`, with the GIS engine installed and `COLUMBUS_PYTHON` set.
4. Say in two lines where things stand.
5. Then run every remaining area in order without stopping between them. Milestones are self-checks you record in the log; you don't wait at them.

## Context

- Write a handoff note at the end of each area, and re-read it before the next.
- After any compaction, re-read these instructions, the newest note and the task list.
- Hand bulky reading, parallel work or reviews to subagents with a complete brief.
- If the session ends early (a time or context limit), the next one starts from the handoff note; the owner only has to say "continue".

## Evidence

A task is done only when every acceptance check passes and the evidence is recorded in `docs/tasks.json` with a line in the log: a pointer (commit, test output, CI run link) plus one line of result. Never weaken a check to pass it; if a check is wrong, log a decision and fix the check only where the brief's intent is unchanged.

Look up current documentation and versions before relying on them; the research is dated 2026-11-08.

## Saving work

Policy (D-1, D-8):
- Each area works on a branch `rework/s<n>-<area>`, with a commit after every task.
- Open a pull request to `main` at the end of the area, and **merge it yourself** once every job in `.github/workflows/ci.yml` passes on every system. If CI fails, fix and push again.
- Never ask the owner to merge.
- Any change can be rolled back by reverting its commits.
- Secrets never go into the repository or its history.
- History is never rewritten, and branches and tags are never deleted.

## When to stop and ask

Settle problems yourself first:
- use the brief's fallback for a false assumption (A-1 to A-6);
- decide two-way details and log them;
- if CI flakes, re-run it;
- if one approach fails twice, try another.

Stop and ask only for something you cannot do or decide: an access only the owner can grant, or a choice that would change a one-way door, the scope, the security rules or the intent. When you must ask:
- batch everything into one message;
- give each question 2–4 options, recommended first with its reason, plus "You decide";
- keep working on every task the question doesn't block.

Never without approval:
- publishing anything except the 0.14 test release (D-7); changing repository visibility, licence or earlier releases;
- deleting branches (including `rustgis-pilot`), tags, releases or files outside this rework, or rewriting history;
- spending money, signing up for services, or adding telemetry;
- using the owner's real connections, accounts, tokens or data;
- adding scope;
- messaging or opening issues on other repositories.

Never ask for passwords, keys or other secrets in the chat; tell the owner where to enter them.

A change touching a one-way door (D-1 to D-5, D-7, D-10, D-12, D-14, F-1 to F-14) is never decided here. Log it as "Sent to Blueprint", mark the affected tasks `blocked` with the reason, and finish the rest. Dependent tasks do what they can without it. The final review and release still happen, and the release notes list what was left out.

## Final review and release

When every task T-1 to T-25 except T-18 is done or blocked, run the final review yourself in a fresh subagent from `docs/final-review.md`.
- Add every Fix-now finding as a task numbered from `R-101`, fix it, then run one re-check in another fresh subagent.
- Log the other findings.
- Then do T-17: dispatch the Release workflow on `main` (D-7).

Finally, send the owner one message:
- the release link;
- what 0.14 can do now;
- the optional checks H-1 and H-2;
- a reminder to start the separate UI-review project.

## Style

Plain words, short sentences: what was done, what you need (usually nothing), what's next.
