import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "../../lib/api";
import { fetchJson } from "../../lib/queries";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { Checkbox, Field, SearchInput, Select } from "../ui/field";
import { Failure, Muted } from "../ui/primitives";

type Person = { id: string; display_name: string; generation: number };
type Preview = { source: Person; target: Person; counts: Record<string, number>; conflicts: string[]; can_merge: boolean };
const labels: Record<string, string> = { accounts: "来源账号", fields: "当前字段", confirmed_faces: "已确认人脸", suggested_faces: "人脸建议", media_links: "媒体关联", dates: "重要日期" };
const conflicts: Record<string, string> = { group_memberships: "群成员身份或入退群时间", reconnect_decisions: "稍后联系决定", suggestion_decisions: "资料建议决定", manual_fields: "人工字段", birthday: "生日", face_decisions: "人脸确认与拒绝", summary: "人工简介", structured_fields: "结构化经历", link_decisions: "账号关联决定", relationship_values: "关系备注或时间" };

export function MergePerson({ sourceId, initialTarget = "", onClose, onMerged }: { sourceId: string; initialTarget?: string; onClose: () => void; onMerged: (id: string) => void }) {
  const [query, setQuery] = useState("");
  const [target, setTarget] = useState(initialTarget);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [operation, setOperation] = useState(() => crypto.randomUUID());
  const people = useQuery({ queryKey: ["merge-people", query], enabled: query.trim().length > 0,
    queryFn: ({ signal }) => fetchJson<{ profile_id: string; display_name: string }[]>(`/api/v1/link-review/contacts?q=${encodeURIComponent(query.trim())}`, { signal }) });
  const preview = useQuery({ queryKey: ["merge-preview", sourceId, target], enabled: !!target,
    queryFn: ({ signal }) => fetchJson<Preview>(`/api/v1/profiles/${sourceId}/merge-preview?target_id=${encodeURIComponent(target)}`, { signal }) });
  const data = preview.data;
  const choices = people.data?.filter(person => person.profile_id !== sourceId) ?? [];
  async function merge() {
    if (!data?.can_merge || !confirmed) return;
    setBusy(true); setError("");
    try {
      const response = await apiFetch(`/api/v1/profiles/${sourceId}/merge`, { method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ operation_id: operation, target_id: target, source_generation: data.source.generation, target_generation: data.target.generation }) });
      if (!response.ok) throw new Error("资料已变化，或仍有冲突。请重新读取预检后再试。");
      onMerged(target);
    } catch (reason) { setError(reason instanceof Error ? reason.message : "合并未完成，请重试。"); }
    finally { setBusy(false); }
  }
  return <Dialog title="合并同一个人的资料" onClose={onClose} locked={busy} description="账号、笔记、头像、人脸归属和历史会保留。合并后原资料会跳转到保留的人物。">
    <Field label="查找要保留的人物"><SearchInput value={query} disabled={busy} onChange={event => setQuery(event.target.value)} placeholder="输入姓名" /></Field>
    {people.isError && <Failure onRetry={() => void people.refetch()}>无法搜索人物。</Failure>}
    <Field label="保留这份人物资料"><Select value={target} disabled={busy} onChange={event => { setTarget(event.target.value); setConfirmed(false); setOperation(crypto.randomUUID()); setError(""); }}>
      <option value="">请选择</option>
      {target && !choices.some(person => person.profile_id === target) && <option value={target}>{data?.target.display_name ?? "已选人物"}</option>}
      {choices.map(person => <option value={person.profile_id} key={person.profile_id}>{person.display_name}</option>)}
    </Select></Field>
    {preview.isFetching && <Muted>正在检查关联内容和冲突…</Muted>}
    {preview.isError && <Failure onRetry={() => void preview.refetch()}>无法读取合并预检。</Failure>}
    {data && <>
      <p>{data.source.display_name} → {data.target.display_name}</p>
      <ul>{Object.entries(data.counts).map(([key, value]) => <li key={key}>{labels[key] ?? key}：{value}</li>)}</ul>
      {data.conflicts.length > 0 ? <p role="alert">请先处理冲突：{data.conflicts.map(key => conflicts[key] ?? key).join("、")}。</p> : <Muted>没有发现需要先处理的人工内容冲突。目标人物的姓名和头像选择会保留。</Muted>}
      <Checkbox checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)}>我确认这两份资料是同一个人</Checkbox>
    </>}
    {error && <p role="alert">{error} <Button size="sm" onClick={() => void preview.refetch()}>重新预检</Button></p>}
    <Button variant="primary" disabled={busy || !confirmed || !data?.can_merge || preview.isFetching} onClick={() => void merge()}>{busy ? "正在合并…" : "合并并保留历史"}</Button>
  </Dialog>;
}
