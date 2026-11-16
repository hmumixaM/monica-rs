import { useInfiniteQuery } from "@tanstack/react-query";
import { fetchJson } from "../../lib/queries";
import { localMediaUrl } from "../../lib/media";
import { Button } from "../ui/button";
import { Failure, Muted, Panel, providerLabel } from "../ui/primitives";
import styles from "./profile.module.css";

type Observation = { id: string; sequence: number; provider: string; previous_media_id: string | null; media_id: string; observed_at: string | null };
export function AvatarHistory({ profileId }: { profileId: string }) {
  const history = useInfiniteQuery({ queryKey: ["avatar-history", profileId], initialPageParam: "",
    queryFn: ({ pageParam, signal }) => fetchJson<Observation[]>(`/api/v1/profiles/${profileId}/avatar-history${pageParam ? `?before=${pageParam}` : ""}`, { signal }),
    getNextPageParam: page => page.length === 40 ? String(page.at(-1)!.sequence) : undefined });
  const items = history.data?.pages.flat() ?? [];
  return <Panel className={styles.tabPanel} label="头像观察记录">
    <h2>头像观察记录</h2><Muted>记录采集时看到的头像。观察时间不等于实际换头像的时间；没有历史证据时只保留当前基线。</Muted>
    {history.isError && <Failure onRetry={() => void history.refetch()}>头像记录未能读取。</Failure>}
    {history.isSuccess && !items.length && <Muted>暂时没有头像观察记录。</Muted>}
    {items.map(item => <article key={item.id} className={styles.historyEntry}>
      <strong>{providerLabel(item.provider)} · {item.previous_media_id ? "观察到不同头像" : "头像基线"}</strong>
      <small>{item.observed_at ? new Date(item.observed_at).toLocaleString("zh-CN") : "历史观察时间未记录"}</small>
      <div className={styles.avatarHistoryImages}>
        {item.previous_media_id && <a href={localMediaUrl(item.previous_media_id)} target="_blank" rel="noreferrer"><img loading="lazy" src={localMediaUrl(item.previous_media_id)} alt="此前观察到的头像" /></a>}
        <a href={localMediaUrl(item.media_id)} target="_blank" rel="noreferrer"><img loading="lazy" src={localMediaUrl(item.media_id)} alt="本次观察到的头像" /></a>
      </div>
    </article>)}
    {history.hasNextPage && <Button disabled={history.isFetching} onClick={() => void history.fetchNextPage()}>读取更早的头像记录</Button>}
  </Panel>;
}
