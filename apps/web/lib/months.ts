/** Normalize current month keys and legacy date-shaped keys at the UI boundary. */
export function monthKey(value: string): string | undefined {
  const match = /^(\d{4})-(\d{2})(?:-\d{2}(?:T.*)?)?$/.exec(value);
  return match && Number(match[2]) >= 1 && Number(match[2]) <= 12 ? `${match[1]}-${match[2]}` : undefined;
}

export const currentMonth = () => new Date().toISOString().slice(0, 7);

export function nextMonth(month: string): string {
  const key = monthKey(month);
  if (!key) throw new Error("Invalid month");
  const [year, value] = key.split("-").map(Number);
  return value === 12 ? `${year + 1}-01` : `${year}-${String(value + 1).padStart(2, "0")}`;
}

/** Every month from the earliest row through the later of the last row and now; gaps are filled with `empty(month)`. */
export function continuousMonths<T extends { month: string }>(rows: T[], empty: (month: string) => T): T[] {
  if (!rows.length) return [];
  const byMonth = new Map(rows.flatMap((row) => { const key = monthKey(row.month); return key ? [[key, {...row, month: key}] as const] : []; }));
  if (!byMonth.size) return [];
  const sorted = [...byMonth.keys()].sort();
  const last = [currentMonth(), sorted.at(-1)!].sort().at(-1)!;
  const result: T[] = [];
  for (let month = sorted[0]; month <= last; month = nextMonth(month)) result.push(byMonth.get(month) ?? empty(month));
  return result;
}

export const monthLabel = (month: string) => {
  const key = monthKey(month);
  return key ? `${key.slice(0, 4)}年${Number(key.slice(5, 7))}月` : "月份未记录";
};
