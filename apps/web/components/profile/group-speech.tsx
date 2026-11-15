"use client";

import Link from "@/router/link";
import {useEffect, useRef, useState} from "react";
import {apiFetch} from "../../lib/api";
import type {PersonRef} from "../../lib/api-types";
import {Button} from "../ui/button";
import {LoadMore} from "../ui/load-more";
import {Dialog} from "../ui/dialog";
import {Select, SearchInput} from "../ui/field";
import {Failure, Loading, Muted, SourceMark, formatDateTime} from "../ui/primitives";
import styles from "./dialogs.module.css";

type Cursor={before:string;before_id:string};
type SpeechMessage={id?:string;time?:string;sender?:string;text?:string;citation?:string|null};
type SpeechRoom={conversation_id:string;group_id?:string|null;name?:string;provider?:string;chat?:string;messages?:SpeechMessage[];next?:Cursor|null};
const roomKey=(room:SpeechRoom)=>room.conversation_id||room.chat||room.group_id||'';

/** Reads imported group speech. Collection and decryption run separately. */
export function GroupSpeechDialog({profileId,groups,onClose}:{profileId:string;groups:PersonRef[];onClose:()=>void}) {
 const request=useRef(0);const abort=useRef<AbortController|null>(null);
 const [query,setQuery]=useState('');const [groupId,setGroupId]=useState('');const [rooms,setRooms]=useState<SpeechRoom[]>([]);
 const [busy,setBusy]=useState(false);const [error,setError]=useState('');const [nextOffset,setNextOffset]=useState<number|null>(null);
 const [active,setActive]=useState({query:'',group:''});
 async function load(search=query,group=groupId,offset=0,cursor:Cursor|null=null,conversation='') {
  const current=++request.current;abort.current?.abort();const controller=new AbortController();abort.current=controller;
  setBusy(true);setError('');
  const params=new URLSearchParams({limit:'50',group_offset:String(offset),...(cursor||{})});
  if(search.trim())params.set('q',search.trim());if(group)params.set('group_id',group);if(conversation)params.set('conversation_id',conversation);
  try {
   const response=await apiFetch(`/api/v1/profiles/${profileId}/group-speech?${params}`,{signal:controller.signal});
   const body=await response.json().catch(()=>({}));if(current!==request.current)return;
   if(!response.ok){setError(typeof body.detail==='string'?body.detail:'群聊记录读取失败，请重试。');return;}
   const incoming=(body.groups||[]) as SpeechRoom[];
   if(cursor){
    setRooms(previous=>previous.map(room=>{
     if(room.conversation_id!==conversation)return room;
     const next=incoming.find(value=>value.conversation_id===conversation);if(!next)return {...room,next:null};
     const messages=new Map([...(room.messages||[]),...(next.messages||[])].map(m=>[m.id||`${m.time}:${m.text}`,m]));
     return {...next,messages:Array.from(messages.values())};
    }));
   }else{
    setRooms(previous=>offset?Array.from(new Map([...previous,...incoming].map(room=>[roomKey(room),room])).values()):incoming);
    setNextOffset(body.next_group_offset??null);setActive({query:search,group});
   }
  }catch{if(current===request.current&&!controller.signal.aborted)setError('群聊记录读取失败，请重试。');}
  finally{if(current===request.current)setBusy(false);}
 }
 useEffect(()=>{setQuery('');setGroupId('');setRooms([]);setNextOffset(null);void load('','');return()=>{request.current++;abort.current?.abort();};},[profileId]);
 const partial=rooms.find(room=>room.next);
 return <Dialog title="群聊发言" size="wide" onClose={onClose} closeLabel="关闭群聊记录">
  <Muted>显示已导入的群聊发言。新采集的内容会在完成导入后出现。</Muted>
  <form className={styles.speechForm} onSubmit={event=>{event.preventDefault();void load();}}>
   {groups.length>0&&<Select aria-label="选择群" value={groupId} onChange={event=>setGroupId(event.target.value)}><option value="">全部群</option>{groups.map(group=><option key={group.profile_id} value={group.profile_id}>{group.name}</option>)}</Select>}
   <SearchInput aria-label="在发言里搜索" value={query} onChange={event=>setQuery(event.target.value)} placeholder="关键词…"/>
   <Button type="submit" variant="primary" disabled={busy}>{busy?'读取中…':'搜索发言'}</Button>
  </form>
  {error&&<Failure onRetry={()=>void load(active.query,active.group)}>群聊记录读取失败，请重试。</Failure>}
  {busy&&!rooms.length&&<Loading>正在读取已导入的群聊发言…</Loading>}
  {rooms.filter(room=>room.messages?.length).map(room=><section className={styles.room} key={roomKey(room)}>
   <h3>{room.provider&&<SourceMark provider={room.provider}/>} {room.group_id?<Link href={`/profiles/${room.group_id}`}>{room.name}</Link>:room.name}</h3>
   <ol>{room.messages?.map((message,index)=><li key={message.id||`${message.time}:${index}`}>
    <header><strong>{message.sender||'群成员'}</strong><time dateTime={message.time}>{formatDateTime(message.time)}</time></header>
    <p>{message.text||'（空消息）'}</p>{message.citation&&<Link href={message.citation}>查看原文</Link>}
   </li>)}</ol>
  </section>)}
  {(partial||nextOffset!==null)&&<LoadMore cursor={partial?`${partial.conversation_id}:${partial.next!.before_id}`:String(nextOffset)} busy={busy} error={!!error}
   onLoad={()=>partial?void load(active.query,active.group,0,partial.next!,partial.conversation_id):void load(active.query,active.group,nextOffset!)}/>}
  {!busy&&!error&&!rooms.length&&<Muted>已导入的记录中没有找到这个人的群聊发言。</Muted>}
 </Dialog>;
}
