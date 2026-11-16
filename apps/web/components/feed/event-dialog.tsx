"use client";

import { Heart, MessageCircle, X } from "lucide-react";
import Link from "../../src/router/link";
import { type CSSProperties, useEffect, useRef, useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";

import { localMediaUrl } from "../../lib/media";
import { fetchJson } from "../../lib/queries";
import { recordText } from "../../lib/record-text";
import { useZoomFrom } from "../../lib/zoom";
import { PhotoPeople } from "../faces/photo-people";
import { Button, IconButton } from "../ui/button";
import { PersonLink } from "../ui/person-link";
import { Avatar, SourceMark } from "../ui/primitives";
import { ChannelsInfo, EventAnnotations, FeedImage, SharedLinkCard, Unavailable, VideoCover } from "./feed-parts";
import styles from "./feed.module.css";
import { InstagramPostDetails } from "./instagram-post";
import { type EventDetail, availableMedia, eventDate, eventKind, type FeedEvent, isVideo, type MediaReading, type SharedLink } from "./types";

/** Post detail: photos on the left, text, places, tags and interactions on the right. Mounted only while open. */
export function EventDialog({ event, origin, onClose }: { event: FeedEvent; origin: HTMLElement | null; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [detail, setDetail] = useState<EventDetail | null>(null);
  const [error, setError] = useState("");
  const close = useZoomFrom(dialog, origin);

  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    const abort = new AbortController();
    setDetail(null);
    setError("");
    fetch(`/api/v1/timeline/${event.id}`, { signal: abort.signal }).then(async (response) => {
      if (!response.ok) throw new Error(String(response.status));
      setDetail(await response.json());
    }).catch((reason: Error) => { if (reason.name !== "AbortError") setError("详情加载失败，请关闭后重试。"); });
    return () => abort.abort();
  }, [event.id]);

  const metadata = detail?.event_metadata || event.event_metadata;
  const allMedia = detail?.media || event.media || [];
  const media = allMedia;
  const link = (detail?.content as { shared_link?: SharedLink } | undefined)?.shared_link || event.shared_link;
  const photoMedia = link ? media.slice(1) : media;
  const body = recordText(detail?.content) || recordText(detail?.summary || event.summary);
  const interactions = detail?.interactions?.filter((item) => item.interaction_type !== "mention") ?? [];
  const hasPhotos = photoMedia.length > 0;
  const ratio = media[0]?.width && media[0]?.height ? media[0].width / media[0].height
    : metadata?.event_type === "story" ? 9 / 16 : undefined;

  return <dialog ref={dialog} className={styles.detail} data-photos={hasPhotos ? "" : undefined} data-source={event.provider}
    style={ratio ? { "--photo-ratio": ratio } as CSSProperties : undefined} aria-label="动态详情" onClose={onClose}
    onCancel={(cancel) => { cancel.preventDefault(); close(); }}
    onClick={(click) => { if (click.target === click.currentTarget) close(); }}>
    <header className={styles.detailHeader}>
      {event.profile_id ? <PersonLink className={styles.person} href={`/profiles/${event.profile_id}`}>
        <Avatar name={event.profile_name || event.title} mediaId={event.avatar_media_id} size={36} round />
        <span><strong>{event.profile_name || event.title}</strong><small><SourceMark provider={event.provider} size={11} /> {eventKind({ ...event, event_metadata: metadata })} · <time dateTime={event.occurred_at}>{eventDate(event.occurred_at, event.provider)}</time></small></span>
      </PersonLink> : <span className={styles.person}><span><strong>{event.title}</strong><small>{eventKind(event)} · {eventDate(event.occurred_at, event.provider)}</small></span></span>}
      <IconButton label="关闭动态详情" onClick={close}><X size={18} /></IconButton>
    </header>
    {hasPhotos && <div className={styles.detailPhotos}>
      {photoMedia.map((item, index) => item.ready !== false && localMediaUrl(item.id)
        ? <a href={localMediaUrl(item.id)} target="_blank" rel="noreferrer" aria-label={`查看原图 ${index + 1}`} key={item.id}>
          {item.media_type?.includes("video")
            ? <video controls preload="metadata" src={localMediaUrl(item.id)} poster={localMediaUrl(item.thumbnail_id)} data-zoom-target={index === 0 ? "" : undefined} />
            : <FeedImage media={item} original alt={`动态照片 ${index + 1}`} zoomTarget={index === 0} />}
        </a>
        : <span key={index}><Unavailable>{`${isVideo(item) ? "视频" : "动态照片"} ${index + 1} · 暂时无法读取`}</Unavailable></span>)}
    </div>}
    <div className={styles.detailText}>
      {link && <SharedLinkCard link={link} media={media[0]} />}
      {body && <p className={styles.body}>{body}</p>}
      <ChannelsInfo media={media[0]} />
      {hasPhotos && <PhotoPeople mediaIds={availableMedia(media).flatMap((item) => !isVideo(item) && item.id && localMediaUrl(item.id) ? [item.id] : [])} />}
      <EventAnnotations metadata={metadata} detailed />
      <InstagramPostDetails post={metadata?.post} content={detail?.content} />
      {detail?.media_reading && <MediaReadingNote reading={detail.media_reading} />}
      {!!detail?.legacy_version_count && <LegacyVersions eventId={event.id} count={detail.legacy_version_count} />}
      {!!detail?.participants?.length && <div className={styles.participants}>{detail.participants.map((person) =>
        <Link key={person.id} href={`/profiles/${person.id}`}>{person.display_name}</Link>)}</div>}
      {!detail && !error && <p className={styles.status} role="status">正在读取详情…</p>}
      {error && <p role="alert">{error}</p>}
      {interactions.length > 0 && <section className={styles.replies}>
        <h3>赞与评论</h3>
        {interactions.map((item) => <div key={item.id}>
          {item.interaction_type === "like" ? <Heart size={13} aria-hidden="true" /> : <MessageCircle size={13} aria-hidden="true" />}
          {item.author_profile_id ? <Link href={`/profiles/${item.author_profile_id}`}>{item.author_name || "查看联系人"}</Link> : <strong>{item.author_name || "用户"}</strong>}
          {item.content?.text && <span>{item.content.text}</span>}
          {!!item.media?.length && <div className={styles.commentPhotos}>
            {(item.media || []).map((image, index) => image.ready !== false && localMediaUrl(image.id)
              ? <a key={image.id} href={localMediaUrl(image.id)} target="_blank" rel="noreferrer" aria-label={`查看评论原图 ${index + 1}`}>
                <FeedImage media={image} original alt={`评论图片 ${index + 1}`} />
              </a>
              : <span key={index} role="status">评论图片待下载</span>)}
          </div>}
        </div>)}
      </section>}
    </div>
  </dialog>;
}

type LegacyVersion = { key: string; content_hash: string; observed_at: string; content_object_id: string; kind: string };
function LegacyVersions({eventId,count}:{eventId:string;count:number}) {
  const [open,setOpen]=useState(false);
  const [preview,setPreview]=useState<{key:string;content:unknown}|null>(null);
  const [error,setError]=useState('');
  const versions=useInfiniteQuery({
    queryKey:['legacy-social-versions',eventId],enabled:open,
    initialPageParam:undefined as {before:string;before_id:string}|undefined,
    queryFn:({pageParam,signal})=>fetchJson<{records:LegacyVersion[];next:{before:string;before_id:string}|null}>(`/api/v1/imports/legacy/social/${eventId}/versions?`+new URLSearchParams({limit:'20',...pageParam}),{signal}),
    getNextPageParam:page=>page.records.length===20?page.next:undefined,
  });
  return <details className={styles.reading} onToggle={event=>setOpen(event.currentTarget.open)}><summary>历史内容与修订 · {count}</summary>
    {versions.isError&&<p role="alert">历史版本读取失败。<Button size="sm" variant="ghost" onClick={()=>void versions.refetch()}>重试</Button></p>}
    {open&&versions.isPending&&<p role="status">正在读取历史版本…</p>}
    {versions.data?.pages.flatMap(page=>page.records).map(row=>{
      const url=`/api/v1/imports/legacy/content/${encodeURIComponent(row.content_object_id)}`;
      return <section key={row.key}><p>{row.kind==='chatlog'?'本次快照修订':'来源历史版本'} · {new Date(row.observed_at).toLocaleString()}</p>
        <Button size="sm" variant="ghost" onClick={()=>{setError('');void fetchJson<unknown>(url).then(content=>setPreview({key:row.key,content})).catch(()=>setError('该版本的原始文件暂时无法读取。'));}}>查看内容</Button> <a href={url} download>下载原文</a>
        {preview?.key===row.key&&<><p className={styles.body}>{recordText(preview.content)}</p><details><summary>原始记录</summary><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere',maxHeight:'40vh',overflow:'auto'}}>{JSON.stringify(preview.content,null,2)}</pre></details></>}
      </section>;
    })}
    {error&&<p role="alert">{error}</p>}
    {versions.hasNextPage&&<Button size="sm" variant="ghost" disabled={versions.isFetchingNextPage} onClick={()=>void versions.fetchNextPage()}>读取更早的版本</Button>}
  </details>;
}

/** Keep the recognition entry next to the post or Story's own information. */
function MediaReadingNote({ reading }: { reading: MediaReading }) {
  return <details className={styles.reading}>
    <summary>图片识别</summary>
    <p>{reading.summary}</p>
    <ol>{reading.items.map((item) => <li key={item.index}>
      <span>{item.category}</span>{item.description}{item.text && <q>{item.text}</q>}
    </li>)}</ol>
  </details>;
}
