"use client";

import {useQuery} from "@tanstack/react-query";
import {fetchJson} from "../../lib/queries";
import Link from "../../src/router/link";
import {EmptyState, Failure, formatDateTime, formatNumber, providerLabel, Section, Skeleton} from "../ui/primitives";
import styles from "./settings.module.css";

type Health = {
  queues: {job_type: string; resource_class: string; status: string; count: number; oldest_due_at: string | null; last_success_at: string | null; expired_leases: number}[];
  workers: {worker_id: string; resource_class: string; heartbeat_at: string; draining: boolean; stale: boolean}[];
  sources: {source: string; phase: string; status: string; stale: boolean; heartbeat_at: string | null; progress_stalled?: boolean; details: {lag_bytes?: number; processed?: number; phase?: string}}[];
};
const labels: Record<string, string> = {queued: "等待处理", running: "执行中", complete: "历史完成", failed: "失败", cancelled: "已取消", success: "本轮成功", unchanged: "没有新数据", busy: "等待来源空闲", pending: "还有待处理数据", paused: "已暂停"};
const names: Record<string,string> = {
  'ai.profile':'人物 Memory 与资料建议', 'ai.profiles':'安排人物分析', 'ai.person_week':'人物每周近况', 'ai.person_weeks':'安排人物每周近况',
  'ai.digest':'每周回顾', 'ai.memory_chunk':'Memory 素材整理', 'ai.mail_chunk':'邮件素材整理', 'ai.mail':'邮件分析',
  'ai.post_media':'动态图片识别', 'ai.image_ocr':'图片文字识别', 'ai.reconnect':'联系建议', 'ai.reconnect_one':'人物联系建议',
  'ai.community':'社群命名', 'ai.community_names':'安排社群命名', 'message.summary':'聊天统计与摘要',
};
function jobName(kind:string) {
  if(names[kind]) return names[kind];
  if(kind.startsWith('source.project.')) return `${providerLabel(kind.slice(15))} 内容转换`;
  if(kind.startsWith('adapter.')) return `${providerLabel(kind.slice(8))} 本机读取`;
  return '后台处理任务';
}
const stamp = (value: string | null) => value ? formatDateTime(value, true) : "尚无记录";

export function TaskStatus() {
  const query = useQuery({queryKey: ["task-health"], queryFn: () => fetchJson<Health>("/api/v1/jobs/status"), refetchInterval: 30_000});
  const data=query.data;
  const active=data?.queues.filter(row=>row.status==='queued'||row.status==='running'||row.status==='failed')||[];
  return <Section title="后台运行状态" className={styles.panel} action={<Link href="/imports">查看导入进度</Link>}>
    <p>这里显示待处理任务和执行器是否在线。任务数量与已导入的数据条数不同。每 30 秒更新。</p>
    {query.isError ? <Failure onRetry={() => void query.refetch()}>无法读取后台状态。</Failure>
      : !data ? <Skeleton height={80} /> : <>
        <div className={styles.statusSummary}>
          <span>等待处理 <strong>{formatNumber(active.filter(r=>r.status==='queued').reduce((n,r)=>n+r.count,0))} 项</strong></span>
          <span>执行中 <strong>{formatNumber(active.filter(r=>r.status==='running').reduce((n,r)=>n+r.count,0))} 项</strong></span>
          <span>失败记录 <strong>{formatNumber(active.filter(r=>r.status==='failed').reduce((n,r)=>n+r.count,0))} 项</strong></span>
        </div>
        {active.length?<ul className={styles.list}>{active.map(row=><li key={`${row.job_type}-${row.status}`}><div className={styles.text}>
          <strong>{jobName(row.job_type)} · {labels[row.status]??row.status} {formatNumber(row.count)} 项</strong>
          {row.oldest_due_at&&<small>最早加入队列：{stamp(row.oldest_due_at)}</small>}
          {row.expired_leases>0&&<small>有 {formatNumber(row.expired_leases)} 项执行授权已过期，等待工作进程接续。已保存的进度会保留。</small>}
        </div></li>)}</ul>:<EmptyState title="当前没有排队或执行中的任务">这不代表历史数据与媒体已全部导入，请以导入中心的核对结果为准。</EmptyState>}
        <h3>执行器与采集器</h3>
        {!data.workers.length&&<p>没有在线执行器。后台可能已暂停；等待中的任务会保留。</p>}
        <ul className={styles.list}>{data.workers.map(row=><li key={row.worker_id}><div className={styles.text}>
          <strong>后台执行器 · {row.stale?'心跳过期，请检查服务':row.draining?'正在收尾':'在线'}</strong><small>最近心跳：{stamp(row.heartbeat_at)}</small>
        </div></li>)}{data.sources.map(row=><li key={`${row.source}-${row.phase}`}><div className={styles.text}>
          <strong>{providerLabel(row.source)} · {row.stale?'心跳过期':labels[row.status]??row.status}</strong>
          <small>最近心跳：{stamp(row.heartbeat_at)}{row.progress_stalled?' · 近期没有推进，请查看来源或导入诊断':''}</small>
          {row.details.processed!==undefined&&<small>本轮已处理 {formatNumber(row.details.processed)} 条</small>}
        </div></li>)}</ul>
        <details className={styles.advanced}><summary>技术诊断：任务标识与历史计数</summary><div><p>历史完成数包含此前安排的任务，不能用来判断本次导入是否结束。</p><ul className={styles.list}>{data.queues.map(row=><li key={`${row.job_type}-${row.status}`}><div className={styles.text}><strong>{jobName(row.job_type)} · {labels[row.status]??row.status} {formatNumber(row.count)} 项</strong><small>{row.job_type} · {row.resource_class}{row.last_success_at?` · 最近完成 ${stamp(row.last_success_at)}`:''}</small></div></li>)}</ul>
        {data.workers.map(row=><div className={styles.text} key={row.worker_id}><small>执行器 {row.worker_id}</small></div>)}</div></details>
      </>}
  </Section>;
}
