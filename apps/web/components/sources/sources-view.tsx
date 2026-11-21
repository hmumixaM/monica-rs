"use client";

import { useEffect, useState } from "react";

import { useRouter, useSearchParams } from "@/router/navigation";
import { ImportProgress } from "../imports/import-progress";
import { Button, ButtonLink } from "../ui/button";
import { EmptyState, Failure, Faint, Page, PageHeader, Section, Skeleton } from "../ui/primitives";
import { useToast } from "../ui/toast";
import { needsAttention, PROVIDERS, useCapabilities, useConnections } from "./api";
import { ConnectionCard } from "./connection-card";
import { AddMailbox, MailInbox, type MailMethod } from "./mail";
import styles from "./sources.module.css";
import { WhatsAppConnect, WhatsAppExport } from "./whatsapp";

const MAIL_RESULT: Record<string, string> = {
  connected: "邮箱已授权，后台开始同步", cancelled: "邮箱授权已取消",
  failed: "邮箱授权没有完成。请检查 Client ID、回调地址和账号权限后重试",
};
const ARCHIVED = ["wechat", "instagram", "linkedin"];
const NATIVE: Record<string, string> = { apple_contacts: "Contacts", apple_messages: "Messages" };

/** Every place Monica collects from. Pick a source on the left; its accounts and settings are on the right. */
export function SourcesView() {
  const params = useSearchParams();
  const router = useRouter();
  const toast = useToast();
  const connections = useConnections();
  const capabilities = useCapabilities();
  const [method, setMethod] = useState<MailMethod>("imap");
  const selected = PROVIDERS.find((source) => source.id === params.get("source")) ?? PROVIDERS[0];
  const accounts = (connections.data ?? []).filter((connection) => connection.provider === selected.id);
  const capability = capabilities.data?.[selected.id];

  useEffect(() => {
    const result = params.get("mail_authorization");
    if (result && MAIL_RESULT[result]) toast.show({ message: MAIL_RESULT[result] });
  }, []);
  const addMail = (value: MailMethod) => {
    setMethod(value);
    if (selected.id !== "mail") router.replace("/sources?source=mail", { scroll: false });
    requestAnimationFrame(() => document.getElementById("add-mailbox")?.scrollIntoView({ block: "start", behavior: "smooth" }));
  };

  return <Page className={styles.page}>
    <PageHeader title="来源" description="聊天、动态、邮件和通讯录从这些地方汇入同一份档案。每个来源单独连接、同步和暂停。" />
    <ImportProgress compact />
    <div className={styles.layout}>
      <nav className={styles.rail} aria-label="来源类型">
        {PROVIDERS.map((source) => {
          const own = (connections.data ?? []).filter((connection) => connection.provider === source.id);
          return <button key={source.id} type="button" aria-label={`查看${source.name}来源`} aria-pressed={source.id === selected.id}
            className={styles.railItem} onClick={() => router.replace(`/sources?source=${source.id}`, { scroll: false })}>
            <source.icon size={18} strokeWidth={1.7} aria-hidden="true" />
            <strong>{source.name}</strong>
            {own.some(needsAttention) ? <b data-attention="">需要处理</b> : own.length > 0 && <b>{own.length} 个账号</b>}
          </button>;
        })}
      </nav>

      <div className={styles.detail}>
        <header className={styles.detailHeader}>
          <selected.icon size={22} strokeWidth={1.6} aria-hidden="true" />
          <div><h2>{selected.name}</h2><p>{selected.detail}</p></div>
          {selected.id === "mail" && <div className={styles.actions}>
            <Button size="sm" onClick={() => addMail("gmail")}>添加 Gmail</Button>
            <Button size="sm" onClick={() => addMail("outlook")}>添加 Outlook</Button>
            <Button size="sm" onClick={() => addMail("imap")}>添加 IMAP</Button>
          </div>}
          {ARCHIVED.includes(selected.id) && <ButtonLink size="sm" href="/imports">同步与导入进度</ButtonLink>}
        </header>

        <Section title="已连接的账号">
          {connections.isPending ? <Skeleton height={140} />
            : connections.isError ? <Failure onRetry={() => void connections.refetch()}>暂时读不到账号状态。</Failure>
              : accounts.length ? <div className={styles.connections}>{accounts.map((connection) =>
                <ConnectionCard key={connection.id} connection={connection} capability={capability} />)}</div>
                : <EmptyState title={`还没有连接${selected.name}`}>{
                  selected.id === "mail" ? "在下面添加一个邮箱，Monica 会只读同步，并把人际往来的邮件归到对应的人。"
                    : selected.id === "whatsapp" ? "在下面连接设备，或导入手机导出的聊天历史。"
                      : NATIVE[selected.id] ? "在 Mac 上的 Monica 里启用后，记录会出现在这里。"
                        : "归档导入完成后，账号会出现在这里。"}</EmptyState>}
        </Section>

        {selected.id === "mail" && <><AddMailbox method={method} onMethod={setMethod} /><MailInbox /></>}
        {selected.id === "whatsapp" && <><WhatsAppConnect /><WhatsAppExport /></>}
        {NATIVE[selected.id] && <Section title={capability?.collection_available ? "在这台 Mac 上启用" : "本机采集不可用"} panel>
          {capability?.collection_available
            ? <Faint>打开菜单栏里的 Monica 设置窗口，在「本机来源」中启用 {NATIVE[selected.id]}。系统授权在这台 Mac 上完成；暂停后已收录的记录会保留。</Faint>
            : <Faint>{capability?.unavailable_reason || "这个部署读不到 Mac 本机的数据。已经收录的记录仍然可以查看。"}</Faint>}
        </Section>}
        {ARCHIVED.includes(selected.id) && <Section title="收集方式" panel>
          <Faint>{selected.name}的数据来自已授权的本地归档和采集器。新内容到达、照片归档和后台分析的进度都在「同步与导入」里。</Faint>
        </Section>}
      </div>
    </div>
  </Page>;
}
