/** Human text for imported calls, money and reminders; never expose source payloads. */
export function recordText(value: unknown): string {
  if (!value) return "";
  let data: unknown = value;
  if (typeof value === "string") {
    try { data = JSON.parse(value); } catch { return value; }
  }
  if (typeof data === "string") return data;
  if (!data || typeof data !== "object" || Array.isArray(data)) return "";
  const row = data as Record<string, unknown>;
  const lines: string[] = [];
  for (const key of ["text", "caption", "body", "description", "memo"]) {
    if (typeof row[key] === "string" && row[key]) lines.push(row[key] as string);
  }
  if (typeof row.amount_cents === "number") lines.push(`${row.direction === "sent" || row.direction === "outgoing" ? "支出" : row.direction === "received" || row.direction === "incoming" ? "收入" : "金额"} ${(row.amount_cents / 100).toFixed(2)} ${row.currency || "CNY"}`);
  if (typeof row.duration_seconds === "number") lines.push(`通话时长 ${Math.floor(row.duration_seconds / 60)} 分 ${row.duration_seconds % 60} 秒`);
  if (typeof row.status_label === "string") lines.push(row.status_label);
  if (typeof row.initial_date === "string") lines.push(`提醒日期 ${row.initial_date.slice(0, 10)}`);
  if (typeof row.frequency_type === "string" && row.frequency_type !== "one_time") lines.push(`重复：${({ year: "每年", month: "每月", week: "每周", day: "每天" } as Record<string,string>)[row.frequency_type] || row.frequency_type}`);
  return [...new Set(lines)].join("\n");
}

/** A chat body as readable text. Shared links and cards arrive as WeChat XML: show their title instead of the markup. */
export function messageText(body: string): string {
  const start = body.search(/<\?xml|<msg[\s>]/);
  if (start < 0) return body;
  const title = /<title>(?:<!\[CDATA\[)?([^<\]]+)/.exec(body)?.[1]?.trim();
  const rest = title ? `[分享] ${title}` : body.slice(start).replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
  return (body.slice(0, start) + rest).trim();
}
