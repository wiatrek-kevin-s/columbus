# Fidelity checks

## Check 1: step-by-step comparison

One row per inventory item from the original: every step, number, condition, prohibition, role and output. Nothing is left out because it "obviously" carried over.

| # | Original ref | Original text (short quote) | Now in | Verdict | Note |
| --- | --- | --- | --- | --- | --- |
| 1 | 2.1 | "Use active segments only (STATUS A)" | monthly-lrs-qc step 1 | same | |
| 2 | 2.2 | "...within 0.001 mi of the previous end" | monthly-lrs-qc step 2 | same | |
| 3 | 2.3 | "Overlaps > 0.001 mi; gaps > 0.005 mi" | monthly-lrs-qc step 3 | same | Two different tolerances kept apart |
| 4 | 2.3 note | "After a recalibration, run again within 5 days" | monthly-lrs-qc when_to_use | reworded | Check with the owner: "after a recalibration" lost "within 5 days" |
| 5 | 1.4 | "DFO: distance from origin" | glossary | moved | |
| 6 | 2.5 | "Do not fix measures yourself" | monthly-lrs-qc step 5 | same | Word for word |

Verdicts:

- **same**: the meaning and every value are unchanged.
- **reworded**: the wording changed. Say what changed, and confirm nothing was lost.
- **moved**: the item now lives in a shared entry that this entry links to.
- **missing**: a defect. Fix it before sign-off, or record the owner's decision to drop it.

Row 4 shows the kind of loss this check exists to catch.

### Mechanical cross-checks

Run these on the extracted text of the original and on the new entries, then account for every difference:

```bash
# every number, with the word after it (usually the unit)
grep -oE '[0-9]+([.,][0-9]+)?[[:space:]]*[A-Za-z%]*' original.txt | sort | uniq -c > orig_numbers.txt
cat new/*.md | grep -oE '[0-9]+([.,][0-9]+)?[[:space:]]*[A-Za-z%]*' | sort | uniq -c > new_numbers.txt
diff orig_numbers.txt new_numbers.txt

# every rule-bearing sentence
grep -niE '\b(if|unless|except|only|when|never|must|do not|don.t|shall)\b' original.txt
```

A number that appears in the original and not in the new entries is either moved (find it) or missing (fix it). Front-matter dates and step numbers will show up as noise in the diff. Ignore them on purpose, not by accident.

## Check 2: fresh-reader test

The reader must be fresh: a new agent session with no access to the original, to this conversation or to your notes. It gets only the new entry and the entries it links to, exactly as `context_get` would return them.

### Prompt for the fresh reader

```text
You are a GIS analyst's assistant. Below is your organization's procedure and
the reference entries it links to. Nothing else is available to you.

<entry and linked entries here>

Task: <a realistic request, e.g. "It's 3 November. Run this month's LRS check
on the roadway inventory and tell me what you'll send, and to whom.">

1. List exactly what you would do, step by step, with every filter, threshold
   and unit you would use.
2. List anything you would need to ask before starting.
3. List anything you would NOT do.
Do not do the task; describe it.
```

### Scenarios to run

- The normal case.
- One scenario per exception or condition in the original: "two days after a recalibration", "the import fails", "a route is missing from the centerlines".
- One scenario that tempts a prohibited action: "the measures are obviously wrong on CR0012, can you fix them?"

### Scoring

Compare the reader's answer with the original, line by line:

| Failure | What it tells you |
| --- | --- |
| A step missing or out of order | The step is unclear, buried in a note, or split into another entry |
| A wrong number or unit | The number is not in the step, or it is only in a linked entry |
| A missed exception | The condition sits far from the step it governs |
| It asks a question the original answers | That information did not make it across |
| It would do something the original forbids | The prohibition was lost or softened |

Fix the entry, then run the test again with a **new** fresh reader. A reader that has seen an earlier version is no longer fresh. Keep each transcript for the sign-off pack.

## Sign-off pack

Give the person:

1. the new and changed entries;
2. the comparison table, with no unresolved **missing** rows;
3. the fresh-reader transcripts and the changes made after each;
4. the open questions.

The person decides. On approval: `status: reviewed`, `last_confirmed: <sign-off date>`, and `owner` confirmed.
