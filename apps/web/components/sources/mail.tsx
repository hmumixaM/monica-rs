"use client";

import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import Link from "@/router/link";
import { Button } from "../ui/button";
import { Field, Input } from "../ui/field";
import { Menu } from "../ui/menu";
import { EmptyState, Failure, Faint, formatDateTime, Section } from "../ui/primitives";
import { Segmented } from "../ui/segmented";
import { useToast } from "../ui/toast";
import { CATEGORY_LABEL, type Evidence, message, request, useRefreshConnections } from "./api";
import styles from "./sources.module.css";

export type MailMethod = "imap" | "gmail" | "outlook";
const METHODS: { value: MailMethod; label: string }[] = [{ value: "imap", label: "IMAP" }, { value: "gmail", label: "Gmail" }, { value: "outlook", label: "Outlook" }];

/** Connect a mailbox with an app password, or through Google or Microsoft sign-in. */
export function AddMailbox({ method, onMethod }: { method: MailMethod; onMethod: (method: MailMethod) => void }) {
  const refresh = useRefreshConnections();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [redirect, setRedirect] = useState("");
  useEffect(() => {
    if (method === "imap") return;
    void request<{ redirect_uri: string }>("/api/v1/mail-oauth/config").then((value) => setRedirect(value.redirect_uri)).catch((failure) => setError(message(failure)));
  }, [method]);
  async function submit(form: HTMLFormElement) {
    const data = new FormData(form);
    setSaving(true); setError("");
    try {
      if (method === "imap") {
        await request("/api/v1/source-connections", {
          provider: "mail", connection_method: "imap", external_account: data.get("email"), display_name: data.get("name"), host: data.get("host"),
          password: data.get("password"), folders: String(data.get("folders")).split(",").map((folder) => folder.trim()).filter(Boolean),
        });
        form.reset(); await refresh(); toast.show({ message: "邮箱已连接，后台开始只读同步" });
      } else {
        const secret = data.get("client_secret");
        const value = await request<{ authorization_url: string }>("/api/v1/mail-oauth/start", { provider: method, client_id: data.get("client_id"), ...(secret ? { client_secret: secret } : {}) });
        location.assign(value.authorization_url);
        return;
      }
    } catch (failure) { setError(message(failure)); }
    setSaving(false);
  }
  return <Section title="添加邮箱" panel id="add-mailbox" action={<Segmented size="sm" label="连接方式" value={method} onChange={onMethod} options={METHODS} />}>
    <form className={styles.form} onSubmit={(event) => { event.preventDefault(); void submit(event.currentTarget); }}>
      {method === "imap" ? <>
        <Faint className={styles.wide}>Gmail 请使用应用专用密码；其他邮箱需要开启 IMAP over TLS。Monica 只读取邮件，不会发送或删除。</Faint>
        <Field label="名称"><Input name="name" placeholder="个人邮箱" required /></Field>
        <Field label="邮箱地址"><Input name="email" type="email" autoComplete="username" required /></Field>
        <Field label="IMAP 服务器"><Input name="host" defaultValue="imap.gmail.com" required /></Field>
        <Field label="应用专用密码"><Input name="password" type="password" autoComplete="new-password" required /></Field>
        <Field label="文件夹（逗号分隔）"><Input name="folders" defaultValue="INBOX" required /></Field>
        <Button type="submit" variant="primary" disabled={saving}>{saving ? "正在连接…" : "连接邮箱"}</Button>
      </> : <>
        <Faint className={styles.wide}>{method === "gmail" ? "使用 Google Cloud 的桌面应用凭据。" : "使用 Microsoft Entra 的公共客户端应用，并登记下面的回调地址。"}授权只读取邮件。</Faint>
        <Field label="Client ID"><Input name="client_id" required autoComplete="off" /></Field>
        {method === "gmail" && <Field label="Client Secret" hint="凭据文件里有的话填写"><Input name="client_secret" type="password" autoComplete="off" /></Field>}
        <Field label="回调地址"><Input value={redirect} readOnly /></Field>
        <Button type="submit" variant="primary" disabled={saving || !redirect}>{saving ? "正在打开授权…" : "前往账号授权"}</Button>
      </>}
    </form>
    {error && <Failure>{error}</Failure>}
  </Section>;
}

const PAGE = 40;
type Filter = "" | "personal" | "transactional" | "marketing" | "pending";
const FILTERS: { value: Filter; label: string }[] = [
  { value: "", label: "全部" }, { value: "personal", label: "人际往来" }, { value: "transactional", label: "事务" },
  { value: "pending", label: "待分类" }, { value: "marketing", label: "已过滤" },
];

/** What the mailboxes collected and how each message was sorted, with a way to correct the sorting. */
export function MailInbox() {
  const [filter, setFilter] = useState<Filter>("");
  const client = useQueryClient();
  const toast = useToast();
  const mail = useInfiniteQuery({
    queryKey: ["mail-inbox", filter], initialPageParam: {} as { before?: string; before_id?: string },
    queryFn: ({ pageParam }) => request<Evidence[]>("/api/v1/evidence?" + new URLSearchParams({
      limit: String(PAGE), include_filtered: "true", provider: "mail", ...(filter ? { category: filter } : {}), ...pageParam,
    })),
    getNextPageParam: (page) => {
      const last = page.at(-1);
      return page.length === PAGE && last ? { ...(last.occurred_at ? { before: last.occurred_at } : {}), before_id: last.id } : undefined;
    },
  });
  const rows = mail.data?.pages.flat() ?? [];
  async function rule(row: Evidence, scope: "message" | "sender", category: "personal" | "marketing") {
    const target = scope === "message" ? row.external_id : row.metadata.participants?.find((person) => person.role === "sender")?.id;
    if (!target) return;
    try {
      const result = await request<{ queued: number }>(`/api/v1/source-connections/${row.connection_id}/mail-rules`, { scope, target, category });
      toast.show({ message: `规则已保存，正在重新整理 ${result.queued} 封相关邮件` });
      await client.invalidateQueries({ queryKey: ["mail-inbox"] });
    } catch (failure) { toast.show({ message: message(failure) }); }
  }
  return <Section title="邮件收录" panel action={<Segmented size="sm" label="邮件类别" value={filter} onChange={setFilter} options={FILTERS} />}>
    {mail.isError && <Failure onRetry={() => void mail.refetch()}>暂时读不到邮件。</Failure>}
    {!mail.isPending && !mail.isError && !rows.length && <EmptyState title="这里还没有邮件">连接邮箱后，收到的邮件会按人际往来、事务和推广分类，只有前两类进入人物档案。</EmptyState>}
    <ul className={styles.inbox} role="list">{rows.map((row) => {
      const sender = row.metadata.participants?.find((person) => person.role === "sender");
      return <li key={row.id}>
        <div>
          <Link href={`/evidence/${row.id}`}>{row.title || "（无标题）"}</Link>
          <span>{sender ? sender.is_self ? "我" : sender.name || sender.id : row.account_name} · {formatDateTime(row.occurred_at)}</span>
        </div>
        <b className={styles.category} data-category={row.category ?? "pending"}>{CATEGORY_LABEL[row.category ?? "pending"]}</b>
        <Menu trigger={<Button size="sm" variant="ghost">调整</Button>} items={[
          { label: "收录这封邮件", onSelect: () => void rule(row, "message", "personal") },
          { label: "始终收录这位发件人", onSelect: () => void rule(row, "sender", "personal") },
          { label: "始终忽略这位发件人", onSelect: () => void rule(row, "sender", "marketing") },
        ]} />
      </li>;
    })}</ul>
    {mail.hasNextPage && <Button onClick={() => void mail.fetchNextPage()} disabled={mail.isFetchingNextPage}>更早的邮件</Button>}
  </Section>;
}
