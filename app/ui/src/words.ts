// Counts in words, for what the page says to a person.

/** A count of rows: "1 row", "12 rows", "1,204 rows". */
export function rowsText(n: number): string {
  return `${n.toLocaleString()} ${n === 1 ? "row" : "rows"}`;
}
