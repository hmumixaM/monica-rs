"use client";

import { ArrowLeft, Link2, MessagesSquare, UserRound } from "lucide-react";
import Link from "../../src/router/link";
import { useRouter, useSearchParams } from "../../src/router/navigation";
import { useDeferredValue, useState } from "react";

import type { ConversationItem } from "../../lib/api-types";
import { useConversations } from "../../lib/queries";
import { ButtonLink, IconButton } from "../ui/button";
import { SearchInput } from "../ui/field";
import { LoadMore } from "../ui/load-more";
import { Avatar, EmptyState, Failure, formatNumber, relativeDays, Skeleton } from "../ui/primitives";
import { Conversation } from "./conversation";
import styles from "./messages.module.css";

/** A conversation without a person or a title falls back to its account id, unless that id is an opaque reference. */
function conversationName(row: ConversationItem) {
  const id = row.external_id.replace(/^(direct|group):/, "");
  return row.display_name || (/^(connection|urn):/.test(id) ? (row.conversation_type === "group" ? "未命名群聊" : "未知联系人") : id);
}

function sourceName(source?: string) {
  return ({ linkedin: "LinkedIn", apple_messages: "短信 / iMessage", whatsapp: "WhatsApp",
    wechat: "微信", instagram: "Instagram", mail: "邮件" } as Record<string, string>)[source ?? ""]
    ?? (source && source !== "legacy" ? source : "");
}

function ThreadHeading({ row, onBack }: { row?: ConversationItem; onBack: () => void }) {
  const name = row ? conversationName(row) : "会话";
  return <div className={styles.threadHeading}>
    <IconButton className={styles.back} label="返回会话列表" onClick={onBack}><ArrowLeft size={18} /></IconButton>
    {row && <Avatar name={name} mediaId={row.avatar_media_id} size={36} round={row.conversation_type === "direct"} />}
    <span className={styles.threadTitle}><h2>{name}</h2>{row && <small>{sourceName(row.source) && `${sourceName(row.source)} · `}{row.conversation_type === "group" ? "群聊" : "私聊"}{!row.profile_id && " · 未关联"} · {formatNumber(row.message_count)} 条</small>}</span>
    {row?.profile_id
      ? <ButtonLink size="sm" variant="ghost" href={`/profiles/${row.profile_id}`}><UserRound size={14} />资料</ButtonLink>
      : row && <ButtonLink size="sm" variant="ghost" href={`/linking?q=${encodeURIComponent(name)}`}><Link2 size={14} />关联资料</ButtonLink>}
  </div>;
}

export function MessagesView() {
  const params = useSearchParams();
  const router = useRouter();
  const selected = params.get("conversation");
  const [text, setText] = useState("");
  const query = useDeferredValue(text);
  const conversations = useConversations(query);
  const rows = conversations.data?.pages.flat() ?? [];
  const current = rows.find((row) => row.id === selected);
  const open = (id?: string) => router.replace(id ? `/messages?conversation=${id}` : "/messages", { scroll: false });

  return <div className={styles.page} data-open={selected ? "" : undefined}>
    <aside className={styles.list} aria-label="会话">
      <header>
        <h1>聊天</h1>
        <SearchInput value={text} onChange={(event) => setText(event.target.value)} placeholder="找会话" aria-label="找会话" data-slash-target="" />
      </header>
      <ul>
        {conversations.isPending && Array.from({ length: 8 }, (_, index) => <li key={index}><Skeleton height={56} /></li>)}
        {conversations.isError && <li><Failure onRetry={() => void conversations.refetch()}>会话读取失败。</Failure></li>}
        {conversations.isSuccess && !rows.length && <li className={styles.none}>{query ? `没有名字包含「${query}」的会话` : "还没有会话"}</li>}
        {rows.map((row) => {
          const name = conversationName(row);
          return <li key={row.id}>
            <button type="button" aria-current={row.id === selected ? "true" : undefined} onClick={() => open(row.id)}>
              <Avatar name={name} mediaId={row.avatar_media_id} size={42} round={row.conversation_type === "direct"} />
              <span className={styles.meta}>
                <span><strong>{name}</strong><time>{relativeDays(row.last_message_at)}</time></span>
                <small>{sourceName(row.source) && <b>{sourceName(row.source)}</b>}{row.conversation_type === "group" && <b>群</b>}{!row.profile_id && <i>未关联</i>}
                  {row.last_message_preview ? withoutSenderId(row.last_message_preview) : `${formatNumber(row.message_count)} 条消息`}</small>
              </span>
            </button>
          </li>;
        })}
        {conversations.hasNextPage && <li><LoadMore cursor={rows.at(-1)?.cursor ?? ""} busy={conversations.isFetchingNextPage}
          error={conversations.isError} onLoad={() => void conversations.fetchNextPage()} label="更多会话" /></li>}
      </ul>
    </aside>
    <section className={styles.pane} aria-label="会话内容">
      {selected
        ? <Conversation key={selected} conversationId={selected} heading={<ThreadHeading row={current} onBack={() => open()} />}
          counterpartId={current?.conversation_type === "direct" ? current.profile_id ?? undefined : undefined} />
        : <EmptyState icon={<MessagesSquare size={28} />} title="选一个会话">
          </EmptyState>}
    </section>
  </div>;
}

// Group message bodies start with the sender's raw WeChat ID ("wxid_abc:"), which is not readable.
export function withoutSenderId(text: string) {
  return text.replace(/^[A-Za-z][\w-]{4,40}:\s*/, "");
}
