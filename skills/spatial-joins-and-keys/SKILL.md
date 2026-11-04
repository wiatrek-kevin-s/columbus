---
name: spatial-joins-and-keys
description: Getting attribute joins and spatial joins right, covering key types, leading-zero padding (CSJ, county FIPS, ZIP), case and whitespace, null keys, one-to-many row blowups, intersects vs within vs contains, boundary effects and reporting the match rate honestly. Use it when asked to "join a table to a layer", "spatial join points to polygons", "match records by ID", "why didn't my join match", "low match rate", or when a join returns too many rows.
---

# Spatial joins and keys

A join that runs is not a join that worked. The usual failures are quiet: keys that
lost their zeros, numbers joined to text, nulls that never match, a right side that repeats
keys and multiplies rows, and points on a boundary that match two polygons. This skill is
about catching those before reporting a result.

## General practice (with or without Columbus)

### 1. Decide what kind of join you need

- **Attribute join**: rows match when a key field is equal on both sides (route id, CSJ,
  FIPS code, parcel id). Fast and exact, if the keys are clean.
- **Spatial join**: rows match when geometries meet a predicate (a point within a county).
  Use it when there is no shared key, or to check that the shared key agrees with location.
- Decide the expected **cardinality** before you run it:
  one-to-one, many-to-one (many crashes to one county), or one-to-many (one route to
  many segments). Write it down; it is how you will know the result is wrong.

### 2. Profile the keys on both sides first

For each key: its type (text or number), its length range, how many distinct values,
how many nulls and empty strings, and whether it repeats on the side that should be unique.
Look at ten real values from each side next to each other.

### 3. Text vs number, and leading zeros

Codes are text, even when they are all digits. Storing them as numbers drops leading zeros,
and spreadsheets do it silently on open.

| Key | Correct form | Typical damage |
| --- | --- | --- |
| TxDOT CSJ (control-section-job) | 9 characters: 4 control + 2 section + 3 job, e.g. `001513077` (shown as `0015-13-077`) | `1513077` after Excel; dashes on one side only |
| County FIPS | 3 characters, e.g. Bexar `029`, Travis `453`, Harris `201` | `29` |
| State + county FIPS (GEOID) | 5 characters, e.g. `48029`, `48453` | `48029` survives, but `06037` (Los Angeles) becomes `6037` |
| State FIPS | 2 characters: Texas `48`, California `06` | `6` |
| Census tract GEOID | 11 characters | Scientific notation in spreadsheets |
| ZIP code | 5 characters | Texas ZIPs start with 7 and survive; New England `02134` becomes `2134` |
| Route ids | As the LRS defines them, e.g. `IH0035`, `SH0071` | `IH35`, `IH 35`, `ih0035` |

Fix: convert both sides to text, then left-pad with zeros to the documented width.
Pad to the width the data dictionary gives, not to "whatever the longest value is".
Keep the original field and add the normalized key as a new field.

ZIP codes are not areas. Census ZCTAs approximate them; a ZIP-to-ZCTA join is many-to-one
and does not cover PO-box-only ZIPs.

### 4. Case, whitespace and formatting

- `'IH0035'`, `'ih0035'` and `'IH0035 '` are three different keys. Fixed-width `CHAR(n)`
  columns in Oracle and SQL Server often carry trailing spaces.
- Normalize the same way on both sides: trim, upper-case, remove separators (`-`, spaces)
  only when the dictionary says they are not significant.
- Watch for look-alike characters in hand-typed keys: `O` vs `0`, `l` vs `1`,
  non-breaking spaces.

### 5. Nulls never match

A null or empty key cannot match anything, and must not be allowed to match another null.
Count them separately from "has a key, no partner". Report them as their own number.

### 6. One-to-many blowups

If a key repeats on the right side, every left row with that key is copied once per
repeat. Sums and counts afterwards are inflated.

- Count rows before and after. More rows out than left rows in means the right side
  repeats keys.
- Decide: is one-to-many intended (keep it, say so), or should the right side be
  deduplicated (pick the current record) or aggregated (sum, count, max) first?
- Never "fix" a blowup by dropping duplicates afterwards without knowing which row was right.

### 7. Spatial predicates and boundaries

| Predicate | True when | Watch out |
| --- | --- | --- |
| `intersects` | The geometries share any point, including boundaries | A point on a shared county line matches both counties; a road crossing three districts matches all three |
| `within` | Left is inside right, and not only on its boundary | A point exactly on a boundary matches nothing |
| `contains` | Right is inside left (the reverse of within) | Same boundary rule, from the other side |
| `touches` | Boundaries meet but interiors do not | Rarely what an assignment needs |
| `crosses`, `overlaps` | Lines crossing; same-dimension partial overlap | For network or overlay questions |

- For "which polygon is this point in", use `within`, and look at points that matched none
  (on a boundary, outside the area, or a CRS mismatch).
- For "which polygon does this line or polygon belong to", a plain predicate matches
  several. Assign by largest overlap, by a representative point inside the feature, or by
  the start point, and say which rule you used.
- Both inputs must be in the same CRS. Check it before you trust a zero-match result.
- Data digitized at different scales do not line up exactly. A point 2 m outside a parcel
  may still belong to it; a small buffer in a projected CRS, or snapping, can help, but say
  that you used a tolerance and what it was.

### 8. Report the match rate honestly

Always give, for the left side:

- total rows, matched, unmatched, and the percentage unmatched;
- how many of the unmatched had a null or empty key;
- rows out vs rows in (one-to-many);
- a few examples of unmatched keys, and the likely reason (padding, case, a code not in
  the lookup, out of area).

Use a left join to see what did not match. An inner join hides unmatched rows. Never drop
unmatched rows without saying how many and why. "Joined 4,812 of 5,000 crashes to segments
(96.2%); 188 had no match, 41 of them with an empty CSJ" is a good report. "Join complete"
is not.

## If the Columbus tools are available

### Look at the keys first

```json
describe_dataset {"connection": "roads-prod", "dataset": "ROADS.ROADWAY_INVENTORY"}
sample_rows {"connection": "roads-prod", "dataset": "ROADS.ROADWAY_INVENTORY", "n": 20}
context_search {"query": "CSJ", "kind": "dataset"}
```

`describe_dataset` gives each field's type. `sample_rows` shows real values: padded or not,
case, nulls. The context pack often says the rule outright, for example "CSJ values from
spreadsheets often lose their leading zeros; pad to 9 before joining".

### Attribute join

```json
vector_op {
  "op": "join",
  "inputs": {"left": "austin:roads",
             "right": {"connection": "austin-ref", "dataset": "surfaces"}},
  "params": {"left_key": "route_id", "right_key": "route_id", "how": "left"}
}
```

- `right_key` defaults to `left_key`. `how` is `left` (default) or `inner`.
- Rows with a null or empty key are never matched, and on a left join they are kept.
- The output keeps the left geometry; the right side's geometry is dropped. Right fields
  whose names clash get a `_right` suffix.
- CSV inputs are read as text, so `"0042"` stays `"0042"`.
- Key options, applied the same way to both sides before matching:
  - `key_as_text: true`: compare as text (whole numbers lose any trailing `.0`).
  - `pad: N`: zero-fill all-digit keys to N characters (`"42"` becomes `"000000042"` with 9).
  - `trim: true`: remove leading and trailing spaces.
  - `ignore_case: true`: compare without letter case.

  Any of them turns the key fields in the output into the normalized text they were joined
  on, and adds a `join.keys_normalized` info check that says what was done.

Checks:

- `join.key_types` (refused): one key is a number and the other is text, so nothing would
  match. Join again with `"key_as_text": true`, and add `"pad": N` if the codes are ones
  that lost their leading zeros.
- `join.unmatched`: message "N of M rows (P%) had no match on <key>", plus
  "; K of those have an empty <key>, which never matches" when there are empty keys.
  `detail` is `{left, matched, empty_keys}`. Severity is `info` at 0 unmatched, `warn`
  below 40%, `fail` at 40% or more.
  - When unmatched keys match once leading zeros are ignored, the `fix` names the call:
    "K of them match once leading zeros are ignored: join again with "key_as_text": true,
    "pad": W".
  - When they match once spaces and letter case are ignored, the fix says to join again with
    `"trim": true, "ignore_case": true`.
  - Other causes (separators such as `-`, a code that is not in the other table) are not
    detected. Compare `sample_rows` from both sides yourself.
- `join.many_to_many` (warn): "The join produced X rows from N: D key values repeat on the
  right side", `detail` `{left, out}`. Deduplicate or aggregate the right side, or confirm
  that one-to-many is intended.

A padded CSJ join, after the first attempt reported lost zeros:

```json
vector_op {
  "op": "join",
  "inputs": {"left": "roads-prod:ROADS.ROADWAY_INVENTORY", "right": "ws://<session>/results/surfaces-<id>.csv"},
  "params": {"left_key": "csj", "key_as_text": true, "pad": 9}
}
```

For other normalization (removing separators, mapping old codes to new), build a cleaned
table with `sql_select` and join its handle; the result is a CSV, so keys stay text. For
example `REPLACE(csj, '-', '')`. Say in the report what you changed.

### Spatial join

```json
vector_op {
  "op": "spatial_join",
  "inputs": {"left": "ws://<session>/results/crashes-<id>.gpkg", "right": "austin-ref:county"},
  "params": {"predicate": "within", "how": "left"}
}
```

- `predicate`: `intersects` (default), `within`, `contains`, `touches`, `crosses`,
  `overlaps`. `how`: `left` (default) or `inner`.
- If the right input is in another CRS, it is reprojected to the left's, with a
  `crs.agreement` info check.
- `join.unmatched` here is `info` or `warn` only, never `fail`: "N of M features (P%) had no
  match (within)". Judge a high rate yourself.
- `join.many_to_many` (warn): "some features match more than one". The fix suggests
  aggregating the right side or using `within`.

### Output size

Before building a join, spatial join or intersect, Columbus counts the rows it would
produce. Past the organization's read limit (default 200,000) it refuses:
`Refused (output.too_large): join would produce N rows, more than the read limit of L`.
Deduplicate or aggregate the right side, or narrow the inputs with `where`. Do not split the
job into pieces to get under the limit without telling the person.

A dataset input bigger than the read limit is refused before any join runs. A handle that
was cut off when it was made carries `input.truncated` into the join result: the match rate
then describes only part of the data.

### Report

Quote `join.unmatched` and `join.many_to_many` messages as they are, add the empty-key
count, the fix you applied (if any), and the provenance id of the join.
