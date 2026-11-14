"use client";
import { useInfiniteQuery } from '@tanstack/react-query';
import { History, RefreshCw } from 'lucide-react';
import Link from '../../src/router/link';
import { fetchJson } from '../../lib/queries';
import { Button } from '../ui/button';
import { EmptyState, Section, providerLabel } from '../ui/primitives';
import { VirtualList } from '../ui/virtual-list';
import styles from './source-evidence.module.css';

type Entry = { id: string; revision: number; title: string; body: string; provider: string; account_name: string; occurred_at: string | null; category: string | null; copy_count: number; metadata: { participants?: { id: string; name?: string; role: string; is_self?: boolean }[] } };
export function EvidenceTimeline({ profileId }: { profileId: string }) {
  const query = useInfiniteQuery({
    queryKey: ['person-source-evidence', profileId], initialPageParam: {} as { before?: string; before_id?: string },
    queryFn: ({pageParam}) => fetchJson<Entry[]>('/api/v1/evidence?' + new URLSearchParams({profile_id:profileId,limit:'40',...pageParam})),
    getNextPageParam: page => {
      const last = page.at(-1);
      return page.length === 40 && last ? { ...(last.occurred_at ? {before:last.occurred_at} : {}), before_id:last.id } : undefined;
    }, staleTime: 30_000,
  });
  const rows = query.data?.pages.flat() || [];
  return <Section title="跨来源往来" panel icon={<History size={16} aria-hidden="true" />} className={styles.timeline}
    action={<Button size="sm" variant="ghost" onClick={() => void query.refetch()} disabled={query.isFetching}><RefreshCw size={14} aria-hidden="true" />刷新往来</Button>}>
    <p className={styles.hint}>邮件、私聊和本机联系人记录按时间排列。只展示已收录的内容。</p>
    {query.isPending && <p role="status">正在读取来源记录…</p>}
    {query.isError && <p role="alert">来源记录暂时无法读取。</p>}
    <VirtualList items={rows.map(row => ({key:row.id,content:<article className={styles.entry}>
      <Link href={`/evidence/${row.id}?revision=${row.revision}`}><strong>{row.title || '消息与资料'}</strong></Link>
      <p className={styles.hint}>{providerLabel(row.provider)} · {row.account_name} · {row.occurred_at ? new Date(row.occurred_at).toLocaleString() : '时间未知'}{row.copy_count > 1 ? ` · ${row.copy_count} 个账号副本` : ''}</p>
      <p className={styles.hint}>{row.metadata.participants?.filter(p => p.role === 'sender').map(p => p.is_self ? '我' : p.name || p.id).join('、')}</p>
      <p className={styles.body}>{row.body.slice(0,400)}{row.body.length>400 ? '…' : ''}</p>
    </article>}))} />
    {!query.isPending && !query.isError && !rows.length && <EmptyState icon={<History size={24} aria-hidden="true" />} title="还没有关联的来源记录。">关联来源账号后，已收录的往来会显示在这里。</EmptyState>}
    {query.hasNextPage && <Button disabled={query.isFetchingNextPage} onClick={() => void query.fetchNextPage()}>读取更早的往来</Button>}
  </Section>;
}
