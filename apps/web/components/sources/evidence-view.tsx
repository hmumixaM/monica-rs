"use client";

import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, Download, Paperclip } from "lucide-react";
import { useState } from "react";

import Link from "@/router/link";
import { Button } from "../ui/button";
import { Failure, formatDateTime, Loading, Page, Panel, providerLabel, Section, SourceMark } from "../ui/primitives";
import { type Evidence, request } from "./api";
import styles from "./sources.module.css";

const LEGACY_ORIGINAL = /^\/api\/v1\/imports\/legacy\/content\/[0-9a-f-]{36}$/i;

/** One collected record as it was received: a mail, a message or a contact card, with its revisions and files. */
export function EvidenceView({ id, revision }: { id: string; revision?: string }) {
  const [original, setOriginal] = useState(false);
  const record = useQuery({
    queryKey: ["evidence", id, revision],
    queryFn: () => request<Evidence>(`/api/v1/evidence/${id}${revision ? `?revision=${encodeURIComponent(revision)}` : ""}`),
  });
  const data = record.data;
  const files = data ? [...(data.metadata.original ? [data.metadata.original] : []), ...(data.metadata.attachments ?? [])] : [];
  return <Page width="narrow" className={styles.evidence}>
    <Link className={styles.back} href="/sources"><ArrowLeft size={14} aria-hidden="true" />来源</Link>
    {record.isPending && <Loading />}
    {record.isError && <Failure onRetry={() => void record.refetch()}>这条记录暂时读不到。</Failure>}
    {data && <>
      <header className={styles.evidenceHeader}>
        <h1>{data.title || "原始记录"}</h1>
        <p><SourceMark provider={data.provider.startsWith("apple") ? "apple" : data.provider} decorative />{providerLabel(data.provider)} · {data.account_name} · {formatDateTime(data.occurred_at)}</p>
        {!!data.metadata.participants?.length && <ul role="list">{data.metadata.participants.map((person, index) =>
          <li key={index}><span>{({ sender: "发件人", recipient: "收件人", cc: "抄送" } as Record<string, string>)[person.role] ?? person.role}</span>{person.is_self ? "我" : person.name || person.id}</li>)}</ul>}
      </header>
      {!!data.versions?.length && <nav className={styles.versions} aria-label="历史修订">
        <Link href={`/evidence/${data.id}?revision=1`} aria-current={data.revision === 1 ? "page" : undefined}>最初版本</Link>
        {data.versions.map((version) => <Link key={version.revision} href={`/evidence/${data.id}?revision=${version.revision}`}
          aria-current={data.revision === version.revision ? "page" : undefined}>版本 {version.revision}</Link>)}
      </nav>}
      <Panel>
        <div className={styles.bodyBar}>
          <span>{original ? "完整原文，含引用和签名" : "正文"}</span>
          <Button size="sm" variant="ghost" onClick={() => setOriginal(!original)}>{original ? "只看正文" : "查看完整原文"}</Button>
        </div>
        <pre className={styles.body}>{(original ? data.raw_body : data.body) || "这条记录还没有收录正文。"}</pre>
      </Panel>
      {(files.length > 0 || (data.metadata.original_content_url && LEGACY_ORIGINAL.test(data.metadata.original_content_url))) && <Section title="文件" panel>
        <ul className={styles.files} role="list">
          {data.metadata.original_content_url && LEGACY_ORIGINAL.test(data.metadata.original_content_url) &&
            <li><a href={data.metadata.original_content_url} download><Download size={14} aria-hidden="true" />下载原始记录</a></li>}
          {files.map((file, index) => <li key={index}>{file.blob
            ? <a href={`/api/v1/evidence/${data.id}/files/${file.blob}?revision=${data.revision}`}><Paperclip size={14} aria-hidden="true" />{file.filename || "附件"}
              {file.size !== undefined && <small>{Math.ceil(file.size / 1024)} KB</small>}</a>
            : <span><Paperclip size={14} aria-hidden="true" />{file.filename || "附件"}<small>还没有下载</small></span>}</li>)}
        </ul>
      </Section>}
      {!!data.events?.length && <Section title="从这封邮件里整理出的安排" panel>
        <ul className={styles.events} role="list">{data.events.map((event, index) => <li key={index}>
          <strong>{event.title}</strong>
          <span>{[event.date_text, event.organization, event.location, event.reference].filter(Boolean).join(" · ")}</span>
          <blockquote>{event.quote}</blockquote>
        </li>)}</ul>
      </Section>}
      {data.copies && data.copies.length > 1 && <p className={styles.copies}>同一封邮件也在：{data.copies.map((copy) =>
        <Link key={copy.id} href={`/evidence/${copy.id}?revision=${copy.revision}`}>{copy.account_name}</Link>)}</p>}
    </>}
  </Page>;
}
