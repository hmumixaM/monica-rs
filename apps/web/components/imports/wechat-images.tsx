import {useState} from "react";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {apiFetch} from "../../lib/api";
import {fetchJson} from "../../lib/queries";
import {Button} from "../ui/button";
import {Dialog} from "../ui/dialog";
import {Field, Select} from "../ui/field";
import {Failure, Section, formatDateTime, formatNumber} from "../ui/primitives";
import styles from "./imports.module.css";

type Counts = Record<string, number>;
type Run = {id: string; account: string; status: string; counts: Counts; discovery_complete: boolean; indexed_files: number; heartbeat_at: string; heartbeat_stale: boolean; error_code?: string};
type Status = {control: {enabled: boolean; paused: boolean}; runs: Run[]};
type Cursor = {after_day: string; after_group: string};
type Daily = {run_id: string; conversation_id: string; day: string; group_name: string; counts: Counts};
type Item = {message_id: string; state: string; quality: string; media_id?: string; attempts: number; updated_at: string};
const stateNames: Record<string, string> = {indexing: "正在扫描图片缓存", running: "正在补齐", paused: "已暂停", complete: "本轮扫描结束", failed: "需要处理", pending: "等待处理", available: "图片可用", no_reference: "原文缺少图片引用", source_missing: "当前缓存未找到", decode_failed: "解码或文件校验失败", source_changed: "消息已变更，需重新核对"};
const qualityNames: Record<string, string> = {original: "原图", high: "高清", standard: "标准", thumbnail: "缩略图", unknown: "清晰度未确认", none: "暂无图片"};

function Coverage({counts, complete}: {counts: Counts; complete: boolean}) {
  return <dl className={styles.counts}>
    <div><dt>{complete ? "本轮图片消息" : "已发现图片消息"}</dt><dd>{formatNumber(counts.total || 0)}</dd></div>
    <div><dt>图片可用</dt><dd>{formatNumber(counts.available || 0)}{complete && (counts.total || 0) > 0 ? ` · ${Math.round((counts.available || 0) / counts.total * 100)}%` : ""}</dd></div>
    {Object.entries(qualityNames).filter(([key]) => key !== "none").map(([key, name]) => <div key={key}><dt>{name}</dt><dd>{formatNumber(counts[`quality_${key}`] || 0)}</dd></div>)}
    {["pending", "no_reference", "source_missing", "decode_failed", "source_changed"].map(key => <div key={key}><dt>{stateNames[key]}</dt><dd>{formatNumber(counts[key] || 0)}</dd></div>)}
  </dl>;
}

export function WechatImages() {
  const client = useQueryClient();
  const status = useQuery({queryKey: ["wechat-images"], queryFn: () => fetchJson<Status>("/api/v1/imports/wechat-images"), refetchInterval: query => query.state.data?.control.enabled && !query.state.data.control.paused ? 15000 : false});
  const [runId, setRunId] = useState("");
  const [cursor, setCursor] = useState<Cursor | null>(null);
  const [selected, setSelected] = useState<Daily | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const run = status.data?.runs.find(r => r.id === runId) || status.data?.runs[0];
  const daily = useQuery({queryKey: ["wechat-image-daily", run?.id, cursor], queryFn: () => fetchJson<{records: Daily[]; next: Cursor | null}>("/api/v1/imports/wechat-images/daily?" + new URLSearchParams({run_id: run!.id, ...cursor})), enabled: !!run});
  async function action(value: string) {
    setBusy(true);
    try {
      const response = await apiFetch("/api/v1/imports/wechat-images", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify({action: value})});
      if (!response.ok) throw new Error("control");
      setError("");
      await client.invalidateQueries({queryKey: ["wechat-images"]});
    } catch {setError("无法更改图片补齐状态，请重试。");}
    finally {setBusy(false);}
  }
  const control = status.data?.control;
  return <Section title="微信群聊图片" className={styles.panel} action={<Button size="sm" variant="ghost" onClick={() => {void status.refetch(); void daily.refetch();}}>刷新</Button>}>
    <p className={styles.note}>覆盖所有群聊的图片消息，包括未关联人物的群。仅从已配置的只读缓存补齐，原版数据保持原样；微信未缓存的历史图片需要先在微信中取得。图片可用不代表已取得原图。</p>
    {status.isError && <Failure onRetry={() => void status.refetch()}>无法读取群聊图片状态。</Failure>}
    {error && <Failure>{error}</Failure>}
    {control && <div className={styles.imageControls}>
      <span className={styles.stage}>{!control.enabled ? "未启用" : control.paused ? "已暂停" : "已允许补齐"}</span>
      <Button size="sm" disabled={busy} variant="secondary" onClick={() => void action(!control.enabled || control.paused ? "enable" : "pause")}>{!control.enabled ? "允许补齐" : control.paused ? "允许继续" : "暂停补齐"}</Button>
      {control.enabled && <Button size="sm" disabled={busy} variant="ghost" onClick={() => void action("disable")}>关闭</Button>}
      <small className={styles.note}>默认关闭。允许后，由已配置的本机任务执行；不会启动采集器。现有导入完成前请保持关闭。</small>
    </div>}
    {!run && <p className={styles.none}>无记录。尚未运行群聊图片补齐，现有媒体导入继续单独显示。</p>}
    {run && <>
      <Field label="查看补齐记录"><Select value={run.id} onChange={event => {setRunId(event.target.value); setCursor(null);}}>{status.data?.runs.map(r => <option key={r.id} value={r.id}>{r.account} · {formatDateTime(r.heartbeat_at)} · {stateNames[r.status] || r.status}</option>)}</Select></Field>
      <p className={styles.note}>{run.heartbeat_stale ? "心跳已过期，请检查本机任务；本轮尚未完成。" : stateNames[run.status] || run.status} · 最近推进 {formatDateTime(run.heartbeat_at)} · 已索引 {formatNumber(run.indexed_files)} 个缓存文件</p>
      {run.error_code && <p className={styles.failed}>任务需要处理：{run.error_code}。已保存的图片和进度会保留。</p>}
      <Coverage counts={run.counts} complete={run.discovery_complete} />
      <p className={styles.note}>按群／日期查看。以下日期按 UTC 汇总，未运行的历史日期为“无记录”。“本轮扫描结束”仍可能包含缺失图片。</p>
      {daily.isError && <Failure onRetry={() => void daily.refetch()}>无法读取分组覆盖情况。</Failure>}
      <ul className={styles.sources}>{daily.data?.records.map(row => <li key={`${row.day}:${row.conversation_id}`}>
        <div className={styles.sourceHead}><strong>{row.group_name}</strong><span>{row.day} UTC</span><Button size="sm" variant="ghost" onClick={() => setSelected(row)}>查看明细</Button></div>
        <Coverage counts={row.counts} complete={run.discovery_complete} />
      </li>)}</ul>
      <div className={styles.actions}><Button size="sm" variant="ghost" disabled={!cursor} onClick={() => setCursor(null)}>首批记录</Button><Button size="sm" variant="ghost" disabled={!daily.data?.next} onClick={() => setCursor(daily.data!.next)}>后续记录</Button></div>
    </>}
    {selected && <ImageItems group={selected} onClose={() => setSelected(null)} />}
  </Section>;
}

function ImageItems({group, onClose}: {group: Daily; onClose: () => void}) {
  const [after, setAfter] = useState<string | null>(null);
  const query = useQuery({queryKey: ["wechat-image-items", group.run_id, group.conversation_id, group.day, after], queryFn: () => fetchJson<{records: Item[]; next: string | null}>("/api/v1/imports/wechat-images/items?" + new URLSearchParams({run_id: group.run_id, conversation_id: group.conversation_id, day: group.day, ...(after ? {after} : {})}))});
  return <Dialog title={`${group.group_name} · ${group.day}`} onClose={onClose} size="wide">
    {query.isError ? <Failure onRetry={() => void query.refetch()}>无法读取图片明细。</Failure> : query.isPending ? <p role="status">正在读取…</p> : <ul className={styles.sources}>{query.data.records.map(item => <li key={item.message_id}><strong>{stateNames[item.state] || item.state} · {qualityNames[item.quality] || item.quality}</strong><code className={styles.imageId}>{item.message_id}</code><small>{formatDateTime(item.updated_at)} · 尝试 {formatNumber(item.attempts)} 次</small></li>)}</ul>}
    <div className={styles.dialogActions}><Button size="sm" variant="ghost" disabled={!after} onClick={() => setAfter(null)}>首批记录</Button><Button size="sm" variant="ghost" disabled={!query.data?.next} onClick={() => setAfter(query.data!.next)}>后续记录</Button></div>
  </Dialog>;
}
