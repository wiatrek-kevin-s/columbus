---
name: sop-conversion
description: Convert Word, PDF or Excel SOPs, procedures, checklists and manuals into context-pack entries (Markdown with YAML front matter) without losing a step, threshold or exception, then prove the result with a step-by-step comparison and a fresh-reader test before a person signs off. Use when someone asks to turn a procedure document, SOP, work instruction, QC checklist or workflow spreadsheet into context pack entries or agent-readable instructions.
---

# SOP conversion

The goal: an agent with only the converted entry does the task exactly as the original procedure says. That includes every step, number, unit, exception and "do not". Shorter is not better. Faithful is better. A conversion is not finished until it passes both fidelity checks (section 6) and a person has signed it off.

## 1. Read the original completely

Get all the text out, including the parts that hide:

| Source | Extract with | Easy to miss |
| --- | --- | --- |
| Word (.docx) | `pandoc -t markdown file.docx`, or python-docx | Tracked changes and comments (ask which version is current), text boxes, headers and footers, footnotes, numbered-list continuations, tables |
| PDF | `pdftotext -layout file.pdf`; check for scanned pages (no text layer) and OCR them | Values that appear only in screenshots, two-column layouts read out of order, appendices |
| Excel (.xlsx) | Every sheet, including hidden sheets, rows and columns; cell comments | Units in column headers, colours or conditional formatting that mean something ("red = fail"), formulas that are the actual rule, merged cells, data validation lists (they are coded domains) |

Then make an **inventory** of the original, numbered as it is numbered:

- every **step**, in order;
- every **number with its unit**: thresholds, tolerances, counts, dates, frequencies;
- every **condition and exception**: "if", "unless", "except", "only", "when", "after a recalibration";
- every **prohibition**: "do not", "never", "must not";
- **roles**: who does it, who approves, who receives the output;
- **inputs and outputs**: datasets, filters, file formats, where results go;
- **references**: "see Appendix B", other procedures, forms.

Ask the person about anything ambiguous, such as conflicting versions, a comment that changes a step, or a screenshot you cannot read. Do not guess.

## 2. The entry template

The front matter is the **card**: what an agent sees in search results. It decides whether the entry gets opened at all. The body is read only when the entry is opened.

```markdown
---
id: monthly-lrs-qc
kind: sop
title: Monthly LRS quality check
summary: Check the roadway inventory's events against route centerlines for gaps, overlaps and measures off the route.
when_to_use: The first week of each month, or after a recalibration.
owner: GIS Data Management
last_confirmed: 2026-10-01
status: draft
source: { file: sources/LRS QC procedure v4.docx, section: "2. Monthly check" }
links: [roadway-inventory, route-centerlines, state-routes]
---

1. Take active segments only (STATUS = 'A').
2. For each route, sort segments by FROM_MEAS and check that each segment starts where the previous one ended, within 0.001 mile.
3. Flag overlaps longer than 0.001 mile and gaps longer than 0.005 mile.
4. Flag any TO_MEAS beyond the route's end measure.
5. Send the findings layer and a summary table to Data Management. Do not fix measures yourself.
```

| Field | Rule |
| --- | --- |
| `id` | Lower-case letters, digits and `-`; unique in the pack; never changed once published, because other entries link to it |
| `kind` | `sop` for procedures (also `role`, `dataset`, `domain`, `lrs`, `glossary`, `note`, `delivery`, `data-group`) |
| `title` | The words a person would search for: "Monthly LRS quality check", not "Procedure 4.2" |
| `summary` | At most two sentences: what the procedure does and on what |
| `when_to_use` | Required for SOPs and roles. The triggers: the task, the schedule, the event ("after a recalibration") |
| `owner` | The team or person who vouches for the content |
| `last_confirmed` | `YYYY-MM-DD`, not in the future. At sign-off, set it to the sign-off date |
| `status` | `draft` until a person signs off, then `reviewed`; `retired` when replaced |
| `source` | The original file and the section this entry came from, so anyone can check it: `{ file: sources/<original name>, section: "<number and heading>" }` |
| `links` | Ids of the shared entries this one relies on |

Keep the original's step numbers in the body when they help tracing, for example "(orig. 2.3)". For a longer template with inputs, outputs and exceptions sections, see [references/templates.md](references/templates.md).

## 3. Split only by use

**Split an entry only when a task usually needs one part without the other.** Long, intricate procedures stay whole. Never trim detail to make an entry shorter.

- One document with a monthly QC and an annual certification: **two entries**. They have different `when_to_use` triggers and are done at different times.
- A 40-step edit procedure whose steps refer back to each other: **one entry**, even if it is long. Splitting it would make the agent stitch pieces back together, and one of them would be missed.
- A procedure with a rarely used recovery branch ("if the import fails, ..."): **keep it in the same entry**, because the person doing the task needs it right then.
- The test: "Would someone doing this task ever need section B without section A?" If no, keep them together.

## 4. Move repeated material into shared entries

Definitions, dataset descriptions, code lists and LRS facts that several SOPs repeat belong in one shared entry each. Link to it instead of copying it:

| Repeated material | Shared entry kind | Example |
| --- | --- | --- |
| Acronyms and terms (CSJ, DFO) | `glossary` | `glossary` |
| What a table holds, its keys and gotchas | `dataset` | `roadway-inventory` |
| Coded values (functional class 1 to 7) | `domain` | `functional-class` |
| Route id format, measure units, calibration | `lrs` | `state-routes` |
| Who does what | `role` | `gis-analyst` |

Rules:

- **Find before you create.** Search the pack for an existing entry first, and reuse its id.
- **Move, do not drop.** The detail goes to the shared entry, and the SOP links to it in `links` and at the step that needs it.
- **Keep the procedure's own numbers in its steps.** A threshold that belongs to this procedure stays in the step, even if a shared entry mentions a similar value. The reader should not have to open another entry to find a number they must act on.
- If the shared entry and the original disagree (different tolerance, a renamed field), stop and ask the owner which one is right.

## 5. Write the steps so an agent can follow them

- Use one action per step, in the original order, numbered.
- Put the threshold and its unit in the step: "gaps longer than 0.005 mile", not "gaps beyond tolerance".
- Write a condition at the step it governs, not in a note at the end.
- Use the names an agent will see in the data: `STATUS = 'A'`, `FROM_MEAS`. Keep the business name too, if the original uses it.
- Keep prohibitions word for word: "Do not fix measures yourself."
- Describe each screenshot as text: what to click or look at, and which values it shows.
- Do not add steps, best practices or opinions that are not in the original. If you think something is missing, write it as a question for the owner.

## 6. The two fidelity checks

Both are required. Templates for each are in [references/fidelity-checks.md](references/fidelity-checks.md).

**Check 1: step-by-step comparison against the original.** Build a table with one row per inventory item (section 1): the original's reference and text, where it now lives (this entry's step, or another entry's id), and the verdict (same, reworded, moved, or missing). Then:

- Compare the numbers mechanically. List every number with its unit in the original and in the new entries, and diff the two lists. Every number must be accounted for.
- List every sentence with "if", "unless", "except", "only", "do not", "never" or "must" in the original, and point to where each one lives now.
- Any row marked **missing** is a defect. Fix it, or record why it was dropped and get the person's agreement.

**Check 2: the fresh-reader test.** Give an agent that has never seen the original or this conversation only the new entry (and the entries it links to). Give it a realistic task, and ask it to say exactly what it would do, step by step, with values, and what it would ask. Then:

- Compare its plan with the original, step by step.
- Run at least one scenario that triggers each exception, such as "it is the week after a recalibration".
- A failure is any missed step, wrong order, wrong number or unit, missed exception, or a question whose answer was in the original. Fix the entry and run the test again, with a new fresh reader each time.

## 7. Sign-off

A person signs off, never the agent. Give the owner (or their delegate):

1. the new entries;
2. the comparison table, with every row resolved;
3. the fresh-reader transcripts and what you changed after each one;
4. your open questions.

When they approve, set `status: reviewed` and `last_confirmed` to the sign-off date, and confirm `owner`. Until then the entry stays `draft`. Put the original document in the pack's `sources/` folder, under the name its `source` field gives.

## 8. If the Columbus tools are available

The server enforces policy. A context pack describes data and procedures, and it never changes permissions. Nothing in an entry can grant access or switch off a check.

### Find existing shared entries before writing new ones

- `context_search {query, kind, limit}` returns cards. `kind` is one of `dataset`, `domain`, `lrs`, `data-group`, `delivery`, `sop`, `role`, `glossary`, `note`, and `limit` is at most 30. Examples: `{"query": "CSJ", "kind": "glossary"}`, `{"query": "roadway inventory", "kind": "dataset"}`, `{"query": "lrs qc", "kind": "sop"}` (to check that the SOP is not already there).
- `context_get {id}` returns `{card, body}`, so you can decide whether to link to the entry or add to it.
- Search ranks a word found in the title or id highest, then the summary or `when_to_use`, then the body. So put the words people will search for in `title` and `when_to_use`.
- `context.no_match` means nothing matched. Try other words before concluding that a shared entry does not exist.
- `describe_data_group {id}` lists the documents a recurring job already uses. A new SOP for that job should be added to the group's `documents` (suggest this to the pack's owner).

### Writing the pack

No Columbus tool writes to the context pack. It is a folder IT manages (`%ProgramData%\Columbus\context`, or `COLUMBUS_CONTEXT`), often a private git repository. Deliver the entries as files for the person, or as a change to the pack's repository if they ask for one.

### Lint every entry

```powershell
columbus-mcp pack lint C:\path\to\pack
```

Run it, or ask the person to run it if you have no shell. It prints `<pack name>: N entries (...)`, then `No problems found.` or one line per problem. These are the messages you will see:

| Message | Fix |
| --- | --- |
| `no front matter (--- ... ---)` | The file must start with a `---` line |
| `front matter is not valid YAML: ...` | Quote values that contain `:` or start with `{`, `[` or `*` |
| `id "X" must be lower-case letters, digits and -` | Rename the id |
| `id "x" is used by another entry` | Pick a unique id, or merge the two entries |
| `title is required` / `summary is required` | Add them |
| `summary is more than two sentences; move detail to the body` | Shorten it. A `.`, `!` or `?` counts as a sentence end when a space and a capital letter (or the end of the text) follow, so decimals such as `0.005 mile` are fine, but an abbreviation before a capital (`approx. Main St`) counts as an extra sentence |
| `owner is required` / `last_confirmed is required` / `status is required` | Add them |
| `last_confirmed "..." is not a date (YYYY-MM-DD)` / `last_confirmed is in the future` | Fix the date |
| `status "..." is not draft, reviewed or retired` | Use one of the three |
| `when_to_use is required for SOPs and roles` | Add the triggers |
| `links to unknown entry "x"` | Fix the id, or write the shared entry first |

Lint checks structure only. It does not check that `source` exists or matches, that steps are faithful, or that `when_to_use` is useful. The two fidelity checks still apply.

### How agents will see the entry

- While `status` is `draft`, `context_get` returns the check `context.draft`: "This entry is a draft no one has reviewed yet". After sign-off the warning goes away.
- Once `last_confirmed` is older than the pack's `review_interval_months` (12 by default), cards and reads carry `context.stale`. The owner re-confirms by checking the entry and updating the date.
- A `retired` entry returns `context.retired`. When you replace an SOP, retire the old entry and link to the new one, instead of deleting the old one.
