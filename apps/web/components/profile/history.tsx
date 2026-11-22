"use client";

import { useInfiniteQuery } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { useMemo, useState } from "react";

import { fetchJson, queryKeys, useRestoreProfileRevision } from "../../lib/queries";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { Field, Select } from "../ui/field";
import { Failure, Loading, Muted, Panel, providerLabel } from "../ui/primitives";
import { VirtualList } from "../ui/virtual-list";
import { fieldLabel, formatDate, summarizeValue, visibleField } from "./fields";
import styles from "./profile.module.css";
import { AvatarHistory } from "./avatar-history";
import type { Revision } from "./types";

const PAGE_SIZE = 40;
const FACT_LABELS: Record<string, string> = { address: "地址", employment: "工作经历", education: "教育经历" };
type StructuredRevision = {
  id: string; fact_type: string; fact_key: string; value: unknown; entity_name?: string | null;
  source_type: string; source_account_id: string | null; observed_at: string; is_conflict: boolean;
};

function fullValue(value: unknown) {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2) ?? "未记录";
}

function CompleteValue({ value }: { value: unknown }) {
  return <details className={styles.historyValue}><summary>查看完整值</summary><pre>{fullValue(value)}</pre></details>;
}

/** Previous values are compared only within the same field and source account. */
export function HistoryView({ profileId }: { profileId: string }) {
  const [includeBookkeeping, setIncludeBookkeeping] = useState(false);
  const [selected, setSelected] = useState<Revision | null>(null);
  const [notice, setNotice] = useState("");
  const restore = useRestoreProfileRevision(profileId);
  const history = useInfiniteQuery({
    queryKey: queryKeys.profileTab(profileId, "history", { paged: true }),
    initialPageParam: "",
    queryFn: ({ pageParam, signal }) => fetchJson<Revision[]>(`/api/v1/profiles/${profileId}/history?` + new URLSearchParams({ limit: String(PAGE_SIZE), ...(pageParam ? { before: pageParam } : {}) }), { signal }),
    getNextPageParam: (page) => page.length === PAGE_SIZE ? page.at(-1)?.cursor : undefined,
  });
  const versions = useMemo(() => history.data?.pages.flat() ?? [], [history.data]);
  const rows = useMemo(() => {
    const older = new Map<string, Revision>();
    return versions.map((entry) => ({ entry, previous: undefined as Revision | undefined })).reverse().map((row) => {
      const key = JSON.stringify([row.entry.field_key, row.entry.source_type, row.entry.source_account_id ?? null]);
      row.previous = older.get(key);
      older.set(key, row.entry);
      return row;
    }).reverse().filter(({ entry }) => includeBookkeeping || entry.source_type === "manual" || entry.field_key === "monica.custom_fields" || entry.operation === "manual_delete" || (entry.source_type === "wechat" && entry.value === null && visibleField({ field_key: entry.field_key, value: true })) || visibleField(entry));
  }, [versions, includeBookkeeping]);

  return <div className={styles.historyPanels}>
    <Panel className={styles.tabPanel} label="字段变更记录">
      <header className={styles.tabHeader}><h2>字段变更记录</h2><small>已加载 {versions.length} 个版本 · 展示 {rows.length} 个</small></header>
      <Muted>恢复会把所选版本写为当前手工值，并保留全部历史。结构化经历在下方单独展示。</Muted>
      <label className={styles.historyToggle}><input type="checkbox" checked={includeBookkeeping} onChange={(event) => setIncludeBookkeeping(event.target.checked)} />显示同步与统计字段</label>
      {notice && <p role="status">{notice}</p>}
      {history.isPending && <Loading />}
      {history.isError && <Failure onRetry={() => void (history.isFetchNextPageError ? history.fetchNextPage() : history.refetch())}>历史读取失败，已加载的记录仍可查看。</Failure>}
      {history.isSuccess && !rows.length && <Muted>{history.hasNextPage ? "这一批没有可展示的字段变更，可继续读取更早版本。" : "没有符合当前显示条件的字段变更。"}</Muted>}
      <VirtualList items={rows.map(({ entry, previous }) => ({ key: entry.id, content: <article className={styles.historyEntry}>
        <div><strong>{fieldLabel(entry.field_key)}</strong><small>{providerLabel(entry.source_type)}{entry.is_conflict ? " · 冲突候选" : ""} · {formatDate(entry.observed_at)}</small></div>
        <div className={styles.diff}>
          <span data-from={previous ? "" : undefined}>{previous ? summarizeValue(previous.value) : history.hasNextPage ? "更早版本尚未加载" : "此前没有同来源记录"}</span>
          <ArrowRight size={13} aria-hidden="true" />
          <span data-to="">{entry.operation === "manual_delete" ? "已删除" : entry.value === null ? "已清空" : summarizeValue(entry.value)}</span>
        </div>
        <div className={styles.historyActions}><CompleteValue value={entry.value} />
          {entry.value != null && <Button size="sm" variant="ghost" onClick={() => { restore.reset(); setSelected(entry); }}>恢复此版本</Button>}
        </div>
      </article> }))} />
      {history.hasNextPage && <Button disabled={history.isFetching} onClick={() => void history.fetchNextPage()}>{history.isFetchingNextPage ? "正在读取…" : "读取更早的字段版本"}</Button>}
    </Panel>
    <AvatarHistory profileId={profileId} />
    <StructuredHistory profileId={profileId} />
    {selected && <Dialog title={`恢复${fieldLabel(selected.field_key)}`} locked={restore.isPending} onClose={() => setSelected(null)}
      description={selected.field_key === "monica.custom_fields" ? "将恢复这个版本中的全部自定义字段。此操作会覆盖当前整组自定义字段，并保留历史版本。" : "将所选旧值设为当前手工值。后续来源同步不会自动覆盖这个手工值。"}>
      <p>{providerLabel(selected.source_type)} · {formatDate(selected.observed_at)}</p>
      <pre className={styles.historyFullValue}>{fullValue(selected.value)}</pre>
      {restore.isError && <Failure>恢复未完成，请重试。人物或来源归属发生变化时，请先刷新历史。</Failure>}
      <div className={styles.editorActions}><Button disabled={restore.isPending} onClick={() => setSelected(null)}>取消</Button>
        <Button variant="primary" disabled={restore.isPending} onClick={() => restore.mutate(selected, { onSuccess: () => { setNotice(`已恢复${fieldLabel(selected.field_key)}，历史版本已保留。`); setSelected(null); } })}>{restore.isPending ? "正在恢复…" : "恢复为当前值"}</Button>
      </div>
    </Dialog>}
  </div>;
}

function StructuredHistory({ profileId }: { profileId: string }) {
  const [factType, setFactType] = useState("");
  const history = useInfiniteQuery({
    queryKey: queryKeys.profileTab(profileId, "structured-history", { fact_type: factType, paged: true }),
    initialPageParam: {} as { before?: string; before_id?: string },
    queryFn: ({ pageParam, signal }) => fetchJson<StructuredRevision[]>(`/api/v1/profiles/${profileId}/structured-history?` + new URLSearchParams({ limit: String(PAGE_SIZE), ...(factType ? { fact_type: factType } : {}), ...pageParam }), { signal }),
    getNextPageParam: (page) => {
      const last = page.at(-1);
      return page.length === PAGE_SIZE && last ? { before: last.observed_at, before_id: last.id } : undefined;
    },
  });
  const rows = history.data?.pages.flat() ?? [];
  return <Panel className={styles.tabPanel} label="结构化资料历史">
    <header className={styles.tabHeader}><h2>结构化资料历史</h2><small>已加载 {rows.length} 个版本</small></header>
    <Muted>这里保留来源记录的地址、工作和教育经历，可按类型查看历次版本。</Muted>
    <Field label="经历类型"><Select aria-label="经历类型" value={factType} onChange={(event) => setFactType(event.target.value)}><option value="">全部类型</option>{Object.entries(FACT_LABELS).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</Select></Field>
    {history.isPending && <Loading />}
    {history.isError && <Failure onRetry={() => void (history.isFetchNextPageError ? history.fetchNextPage() : history.refetch())}>结构化历史读取失败，已加载的记录仍可查看。</Failure>}
    {history.isSuccess && !rows.length && <Muted>还没有这类结构化资料历史。</Muted>}
    <VirtualList key={factType} items={rows.map((entry) => ({ key: entry.id, content: <article className={styles.historyEntry}>
      <div><strong>{FACT_LABELS[entry.fact_type] ?? entry.fact_type}{entry.entity_name ? ` · ${entry.entity_name}` : ""}</strong><small>{providerLabel(entry.source_type)}{entry.is_conflict ? " · 冲突候选" : ""} · {formatDate(entry.observed_at)}</small></div>
      <p className={styles.historySummary}>{summarizeValue(entry.value)}</p><CompleteValue value={entry.value} />
    </article> }))} />
    {history.hasNextPage && <Button disabled={history.isFetching} onClick={() => void history.fetchNextPage()}>{history.isFetchingNextPage ? "正在读取…" : "读取更早的结构化版本"}</Button>}
  </Panel>;
}
