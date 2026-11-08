# Progress log: Columbus 0.14.0 rework

Kit version 1.1 · as of 2026-11-08, session S1 not started

## Where things stand

- **Session:** S1 in progress · **Task:** T-2
- **Next step:** T-2, the thin `files` connection (CSV)
- **Waiting on the owner:** nothing
- **Homework received:** setup round answered ("skip" on Dependabot alerts); H-1 and H-2 are optional, after release

## Setup round (T-1), 2026-11-08

| Access | Result |
|---|---|
| Push a branch | ok (`rework/s1-files` pushed) |
| Read Actions runs | ok (`gh run list` / `gh run view`) |
| Dispatch a workflow | ok (CI dispatched on `rework/s1-files`, run 42514607603) |
| Merge to `main` | ok: admin rights; no branch protection and no rulesets on `main` |
| Dependabot alerts | off; this session can't change repository security settings. Owner asked once and replied "skip" |

## Decisions

| Date | Decision | Why | Asked or decided |
|---|---|---|---|
| 2026-11-08 | Dependabot alerts stay off for this build. T-15 still adds `.github/dependabot.yml` (version updates don't need alerts) and settles A-4 from the docs | Owner's reply to the setup round | Asked (owner: "skip") |
| 2026-11-08 | `hide_fields` on a `files` connection means the policy's `hide_fields`, as today; no per-connection field is added | F-2 says "as today", and today only policy has `hide_fields`. Reading it as a new connection field would add a format (F-2), so the literal reading is kept. Not a one-way change | Decided |
| 2026-11-08 | `.xls` files are listed under `skipped` with "save as .xlsx" and refused if named | Meets REQ-1's failure case and tells the analyst why the file is missing | Decided |
| 2026-11-08 | Skipped files show only if their would-be dataset name passes the allow list | `skipped` must not reveal more of the folder than the datasets do | Decided |
| 2026-11-08 | XLSX sheets and KML layers are always named `file/layer`, even with one sheet | F-1 treats these as multi-layer formats; names stay stable when a sheet is added | Decided |
| 2026-11-08 | XLSX is read by a small standard-library reader, not GDAL | GDAL treated a header-only sheet's header as data and lost date-time detail, breaking R-6 and R-6b | Decided |
| 2026-11-08 | Unreadable files are listed under `skipped` with the cause; naming one gives a plain error naming the file | REQ-4's failure case without one bad file breaking the whole folder | Decided |
| 2026-11-08 | `crs.present` is a warn for `files` datasets in describe and query; `export_output` still fails a layer with no CRS unless overridden | R-1 and R-2 ask for a warn. Exports check the layer, not its source, and other types stay unchanged (no rule loosens) | Decided |
| 2026-11-08 | Python baseline is 149 passed, not the audit's 115 | `main` moved past the audit commit 8b6bf173 before the kit upload; more tests and no failures, so no action | Decided |

## Change requests

| ID | Date | What and why | Options | Recommendation | Status |
|---|---|---|---|---|---|

Status: raised, approved, declined, or Sent to Blueprint (touches a one-way door; affected tasks paused).

## Handoff notes

Newest first, written as you go. Mark each claim verified (checked this session) or unverified.

### Task log

- 2026-11-08 T-1 done: access checked (verified); baseline at 9749c8f: 287 Rust and 149 Python passed, 0 failures, only database skips (verified); CI run https://github.com/wiatrek-kevin-s/columbus/actions/runs/42514607603 green on 13 jobs (verified).
- 2026-11-08 T-2 done: commit b12ef7d; files.rs 2/2 pass (verified); workspace 292 passed, pytest 159 passed. Left for T-3: R-7 SHA compare, other formats, `file` alias, `crs.present` as warn (today it is a fail).
- 2026-11-08 T-3 done: commit 4bfc56c; files.rs 4/4, test_files.py 22/22 (verified); workspace 295, pytest 171. A-5 confirmed: read_parquet keeps CRS from `geo` metadata.

### Kit → Session S1 (2026-11-08)

- **Done:** nothing yet. The audit baseline on `main` at 8b6bf173: 287 Rust tests and 115 Python tests passed with 0 failures in a temporary copy (unverified for you; re-run in T-1).
- **Decisions and deviations:** none.
- **Loose ends:** assumptions A-1 to A-6 in the brief are open; each names the task that settles it. AF-17 (M geometry) was reproduced in the audit; T-19 fixes it.
- **Saved:** nothing yet.
- **Next:** T-1. Commit `dbe0f20` is the model for adding a connection kind (T-2).
