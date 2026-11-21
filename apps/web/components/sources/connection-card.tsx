"use client";

import { Pause, Play, RefreshCw, Unplug } from "lucide-react";
import { useState } from "react";

import { Button } from "../ui/button";
import { Failure, Faint, formatDateTime, formatNumber, SourceMark } from "../ui/primitives";
import { useToast } from "../ui/toast";
import { CATEGORY_LABEL, type Capability, type Connection, message, needsAttention, request, STATE_LABEL, useRefreshConnections } from "./api";
import { ConnectionPeople } from "./people";
import styles from "./sources.module.css";

const SCOPE_LABEL: Record<string, string> = { message: "单封邮件", thread: "邮件会话", sender: "发件人" };
const day = (value?: string) => (value ? new Date(value).toLocaleDateString("zh-CN") : "未知");

function MailRules({ connection }: { connection: string }) {
  const [rules, setRules] = useState<{ scope: string; target: string; category: string }[] | null>(null);
  const [error, setError] = useState("");
  return <details className={styles.fold} onToggle={(event) => {
    if (event.currentTarget.open) void request(`/api/v1/source-connections/${connection}/mail-rules`).then(setRules).catch((failure) => setError(message(failure)));
  }}>
    <summary>收录规则</summary>
    <Faint>在「邮件收录」里对一封邮件或一位发件人选择包含或忽略，规则会出现在这里。</Faint>
    {error && <Failure>{error}</Failure>}
    {rules?.length ? <ul className={styles.rules} role="list">{rules.map((rule, index) =>
      <li key={index}><span>{SCOPE_LABEL[rule.scope] || rule.scope}</span><code>{rule.target}</code><b>{CATEGORY_LABEL[rule.category] || rule.category}</b></li>)}</ul>
      : rules && <Faint>还没有自定义规则。</Faint>}
  </details>;
}

/** One connected account: what it has collected, whether it is running, and what can be done with it. */
export function ConnectionCard({ connection: c, capability }: { connection: Connection; capability?: Capability }) {
  const refresh = useRefreshConnections();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [people, setPeople] = useState(false);
  const collectable = capability?.collection_available !== false;
  async function act(action: "refresh" | "pause" | "resume" | "disconnect", done: string) {
    setBusy(true);
    try { await request(`/api/v1/source-connections/${c.id}/action`, { action }); await refresh(); toast.show({ message: `${c.display_name}：${done}` }); }
    catch (failure) { toast.show({ message: message(failure) }); }
    finally { setBusy(false); }
  }
  return <article className={styles.connection} data-attention={needsAttention(c) ? "" : undefined}>
    <header>
      <SourceMark provider={c.provider === "apple_messages" || c.provider === "apple_contacts" ? "apple" : c.provider} size={16} decorative />
      <div><h3>{c.display_name}</h3><span>{c.external_account}</span></div>
      <b className={styles.state} data-state={c.state}>{STATE_LABEL[c.state] || c.state}</b>
    </header>
    <dl>
      <div><dt>已收录</dt><dd>{c.records === undefined ? "等待同步" : `${formatNumber(c.records)} 条`}{c.pending_records ? `，${formatNumber(c.pending_records)} 条待整理` : ""}</dd></div>
      {(c.earliest_at || c.latest_at) && <div><dt>时间范围</dt><dd>{day(c.earliest_at)} — {day(c.latest_at)}</dd></div>}
      {c.last_success_at && <div><dt>上次同步</dt><dd>{formatDateTime(c.last_success_at)}{c.sync_pending ? "，后台还在继续" : ""}</dd></div>}
    </dl>
    {c.progress && <Faint>{c.progress}</Faint>}
    {c.coverage && <Faint>{c.coverage}</Faint>}
    {c.error && <Failure>{c.error}</Failure>}
    {c.state !== "legacy" && <div className={styles.actions}>
      <Button size="sm" disabled={busy || !collectable} onClick={() => void act("refresh", "已开始同步")}><RefreshCw size={13} aria-hidden="true" />立即同步</Button>
      <Button size="sm" disabled={busy || (!c.enabled && !collectable)} onClick={() => void act(c.enabled ? "pause" : "resume", c.enabled ? "已暂停" : "已继续")}>
        {c.enabled ? <><Pause size={13} aria-hidden="true" />暂停</> : <><Play size={13} aria-hidden="true" />继续</>}</Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={() => void act("disconnect", "已断开连接")}><Unplug size={13} aria-hidden="true" />断开连接</Button>
    </div>}
    <details className={styles.fold} onToggle={(event) => setPeople(event.currentTarget.open)}>
      <summary>管理人物关联</summary>
      {people && <ConnectionPeople connection={c.id} />}
    </details>
    {c.provider === "mail" && <MailRules connection={c.id} />}
  </article>;
}
