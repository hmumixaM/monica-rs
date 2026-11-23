"use client";

import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { ArrowUp, Check, CircleAlert, Download, Ellipsis, MessageSquarePlus, Search, Sparkles, Square, Trash2, Upload, X } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { useSearchParams } from "@/router/navigation";
import { chooseModel, exportConversation, forget, importConversation, modelChoices, restore, run, runningConversations, snapshot, stop, subscribe } from "../../lib/agent/runtime";
import {
  activeConversation, type Conversation, createConversation, listConversations, saveConversation, selectConversation,
} from "../../lib/agent/sessions";
import { TOOL_LABELS } from "../../lib/agent/tools";
import { fetchJson } from "../../lib/queries";
import { Button, IconButton } from "../ui/button";
import { Select } from "../ui/field";
import { Markdown } from "../ui/markdown";
import { Menu } from "../ui/menu";
import { PersonPicker, type PickedPerson } from "../ui/person-picker";
import { Avatar, Failure, Page, PageHeader } from "../ui/primitives";
import styles from "./ask.module.css";

const STARTERS = ["我最近和谁联系得最多？", "有哪些很久没联系、值得问候的老朋友？", "接下来一个月有谁过生日？"];
const PERSON_STARTERS = ["TA 最近在忙什么？", "我们上次聊了什么？", "我答应过 TA 什么还没做的事？"];

type Step = { id: string; label: string; detail: string; failed: boolean; done: boolean };
type Turn = { question: string; steps: Step[]; answer: string };

/** A question, what Monica looked up for it, and what it said. */
function turns(messages: AgentMessage[]): Turn[] {
  const result: Turn[] = [];
  const steps = new Map<string, Step>();
  for (const message of messages) {
    if (message.role === "user") {
      result.push({ question: typeof message.content === "string" ? message.content : message.content.map((part) => part.type === "text" ? part.text : "").join(""), steps: [], answer: "" });
      continue;
    }
    const turn = result.at(-1);
    if (!turn) continue;
    if (message.role === "toolResult") {
      const step = steps.get(message.toolCallId);
      if (step) { step.done = true; step.failed = message.isError; }
    } else if (message.role === "assistant") {
      for (const part of message.content) {
        if (part.type === "text") turn.answer += part.text;
        if (part.type === "toolCall" && message.stopReason !== "error" && message.stopReason !== "aborted") {
          const args = part.arguments as Record<string, unknown>;
          const step = { id: part.id, label: TOOL_LABELS[part.name] ?? part.name, detail: String(args.query ?? args.since ?? ""), failed: false, done: false };
          steps.set(part.id, step); turn.steps.push(step);
        }
      }
    }
  }
  return result;
}

function Steps({ steps, live }: { steps: Step[]; live: boolean }) {
  if (!steps.length) return null;
  return <details className={styles.steps} open={live || undefined}>
    <summary><Search size={13} aria-hidden="true" />查了 {steps.length} 处资料</summary>
    <ol>{steps.map((step) => <li key={step.id} data-failed={step.failed ? "" : undefined} data-pending={!step.done && live ? "" : undefined}>
      {step.failed ? <CircleAlert size={13} aria-hidden="true" /> : <Check size={13} aria-hidden="true" />}
      {step.label}{step.detail && <q>{step.detail}</q>}{step.failed && <span>查询失败</span>}
    </li>)}</ol>
  </details>;
}

export function AskView() {
  const params = useSearchParams();
  const [id, setId] = useState("default");
  const [sessions, setSessions] = useState<Conversation[]>([]);
  const [ready, setReady] = useState(false);
  const [people, setPeople] = useState<PickedPerson[]>([]);
  const [question, setQuestion] = useState("");
  const [failure, setFailure] = useState("");
  const [picking, setPicking] = useState(false);
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("");
  useEffect(() => { void modelChoices().then((value) => { setModels(value.models); setModel(value.chosen); }).catch(() => undefined); }, []);
  const fileInput = useRef<HTMLInputElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const state = useSyncExternalStore(subscribe, () => snapshot(id), () => snapshot(id));
  const running = useSyncExternalStore(subscribe, runningConversations, runningConversations);
  const transcript = useMemo(() => turns(state.messages), [state.messages]);
  const busy = !ready || state.running;
  const report = (error: unknown) => setFailure(error instanceof Error ? error.message : String(error));

  async function open(session: Conversation) {
    setReady(false); setQuestion(""); setFailure("");
    try {
      await restore(session.id); await selectConversation(session.id);
      setId(session.id); setPeople(session.people);
    } finally { setReady(true); }
  }
  useEffect(() => {
    let active = true;
    void Promise.all([listConversations(), activeConversation()]).then(async ([rows, selected]) => {
      const session = rows.find((row) => row.id === selected) ?? rows[0];
      await restore(session.id);
      if (active) { setSessions(rows); setId(session.id); setPeople(session.people); setReady(true); }
    }).catch(report);
    return () => { active = false; };
  }, []);
  // A finished answer may have renamed the conversation.
  useEffect(() => { if (ready) void listConversations().then(setSessions).catch(report); }, [running, ready]);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [state.messages, state.status]);

  async function choosePeople(next: PickedPerson[]) {
    setPeople(next);
    const session = (await listConversations()).find((row) => row.id === id)!;
    await saveConversation({ ...session, people: next, updatedAt: Date.now() });
  }
  // Arriving from a profile ("问 Monica 关于 TA") starts with that person selected.
  const fromProfile = params.get("profile");
  const applied = useRef<string | null>(null);
  useEffect(() => {
    if (!fromProfile || !ready || applied.current === fromProfile) return;
    applied.current = fromProfile;
    void fetchJson<{ id: string; display_name: string; avatar_media_id?: string | null }>(`/api/v1/profiles/${encodeURIComponent(fromProfile)}`)
      .then((profile) => choosePeople([{ id: profile.id, display_name: profile.display_name, avatar_media_id: profile.avatar_media_id ?? null }]))
      .catch(report);
  }, [fromProfile, ready, id]);

  // A question typed on the home page arrives ready to send.
  const handedOver = params.get("q");
  const appliedQuestion = useRef<string | null>(null);
  useEffect(() => {
    if (!handedOver || !ready || appliedQuestion.current === handedOver) return;
    appliedQuestion.current = handedOver;
    setQuestion(handedOver); input.current?.focus();
  }, [handedOver, ready]);

  async function startNew() {
    setReady(false);
    try { const session = await createConversation(); setSessions(await listConversations()); await open(session); input.current?.focus(); }
    catch (error) { setReady(true); report(error); }
  }
  async function remove(session: Conversation) {
    try {
      await forget(session.id);
      const rest = await listConversations();
      setSessions(rest);
      if (session.id === id) await open(rest[0]);
    } catch (error) { report(error); }
  }
  async function importFile(file: File) {
    try {
      const session = await createConversation(); await importConversation(session.id, file);
      const rows = await listConversations(); setSessions(rows); await open(rows.find((row) => row.id === session.id)!);
    } catch (error) { report(error); }
  }
  function ask() {
    const value = question.trim();
    if (!value || busy) return;
    setQuestion(""); setFailure("");
    void run(id, value, people);
  }
  function onKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); ask(); }
  }
  const fill = (text: string) => { setQuestion(text); input.current?.focus(); };
  const current = sessions.find((session) => session.id === id);

  return <Page width="wide" className={styles.page}>
    <PageHeader title="问 Monica" description="问一个人的近况、聊过的事，或者整体的往来。回答里的链接可以打开原文。" />
    <div className={styles.workspace}>
      <aside className={styles.sessions} aria-label="会话">
        <Button block variant="secondary" disabled={!ready} onClick={() => void startNew()}><MessageSquarePlus size={15} aria-hidden="true" />新会话</Button>
        <ul role="list">{sessions.map((session) => <li key={session.id} data-current={session.id === id ? "" : undefined}>
          <button type="button" disabled={!ready} aria-current={session.id === id ? "true" : undefined}
            onClick={() => { if (session.id !== id) void open(session).catch(report); }}>{session.title}</button>
          {sessions.length > 1 && <IconButton size="sm" label={`删除会话：${session.title}`} disabled={!ready || running.has(session.id)}
            onClick={() => void remove(session)}><Trash2 size={13} /></IconButton>}
        </li>)}</ul>
      </aside>

      <section className={styles.chat} aria-label="当前会话">
        <header className={styles.chatHeader}>
          <Select className={styles.sessionSelect} aria-label="选择会话" value={id} disabled={!ready} onChange={(event) => {
            const selected = sessions.find((session) => session.id === event.target.value);
            if (selected) void open(selected).catch(report);
          }}>{sessions.map((session) => <option key={session.id} value={session.id}>{session.title}</option>)}</Select>
          <h2>{current?.title ?? "新会话"}</h2>
          <IconButton className={styles.newMobile} label="新会话" disabled={!ready} onClick={() => void startNew()}><MessageSquarePlus size={17} /></IconButton>
          <Menu trigger={<IconButton label="会话操作" disabled={!ready}><Ellipsis size={17} /></IconButton>} items={[
            { label: "导出会话", icon: <Download size={15} />, onSelect: () => void exportConversation(id).catch(report) },
            { label: "导入会话", icon: <Upload size={15} />, disabled: state.running, onSelect: () => fileInput.current?.click() },
          ]} />
          <input ref={fileInput} hidden type="file" aria-label="导入会话文件" accept="application/json" onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void importFile(file);
            event.target.value = "";
          }} />
        </header>

        <div className={styles.transcript} aria-live="polite">
          {!transcript.length && <div className={styles.welcome}>
            <Sparkles size={22} aria-hidden="true" />
            <strong>{people.length ? `想了解${people.map((person) => person.display_name).join("、")}的什么？` : "想知道什么？"}</strong>
            <p>{people.length ? "Monica 会翻看聊天、动态和已整理的资料来回答。" : "直接提问，或者先在下面选一个人，问题就会围绕 TA。"}</p>
            <div className={styles.starters}>{(people.length ? PERSON_STARTERS : STARTERS).map((text) =>
              <button key={text} type="button" disabled={!ready} onClick={() => fill(text)}>{text}</button>)}</div>
          </div>}
          {transcript.map((turn, index) => {
            const live = state.running && index === transcript.length - 1;
            return <article key={index} className={styles.turn}>
              <p className={styles.question}>{turn.question}</p>
              <div className={styles.answer}>
                <Steps steps={turn.steps} live={live} />
                {turn.answer.trim() && <Markdown>{turn.answer}</Markdown>}
                {live && <p className={styles.status} role="status"><span aria-hidden="true" />{state.status}</p>}
              </div>
            </article>;
          })}
          {(state.error || failure) && <Failure>{state.error || failure}</Failure>}
          <div ref={end} />
        </div>

        <form className={styles.composer} onSubmit={(event) => { event.preventDefault(); ask(); }}>
          <div className={styles.people}>
            {people.map((person) => <span key={person.id} className={styles.person}>
              <Avatar name={person.display_name} mediaId={person.avatar_media_id} size={20} round />{person.display_name}
              <button type="button" aria-label={`移除人物 ${person.display_name}`} disabled={busy}
                onClick={() => void choosePeople(people.filter((other) => other.id !== person.id)).catch(report)}><X size={12} /></button>
            </span>)}
            {picking
              ? <div className={styles.picker}><PersonPicker autoFocus disabled={busy} placeholder="搜索人物…" onPick={(person) => {
                setPicking(false);
                if (!people.some((other) => other.id === person.id)) void choosePeople([...people, person]).catch(report);
              }} /><IconButton size="sm" label="取消选择人物" onClick={() => setPicking(false)}><X size={14} /></IconButton></div>
              : <button type="button" className={styles.addPerson} disabled={busy} onClick={() => setPicking(true)}>@ {people.length ? "再加一个人" : "选择人物"}</button>}
          </div>
          <div className={styles.inputRow}>
            <textarea ref={input} id="monica-question" name="question" aria-label="问题" rows={2} value={question} disabled={!ready} onKeyDown={onKey}
              onChange={(event) => setQuestion(event.target.value)} placeholder={people.length ? "问关于 TA 的问题…" : "输入问题…"} />
            {state.running
              ? <IconButton label="停止回答" className={styles.send} onClick={() => stop(id)}><Square size={15} /></IconButton>
              : <button type="submit" className={styles.send} aria-label="提问" title="提问（Enter）" disabled={!ready || !question.trim()}><ArrowUp size={17} /></button>}
          </div>
          <div className={styles.footnote}>
            <small>Enter 发送，Shift + Enter 换行。会话只保存在这个浏览器里。</small>
            {models.length > 1 && <label className={styles.model}>模型
              <select aria-label="回答用的模型" value={state.running ? state.model ?? model : model} disabled={state.running}
                onChange={(event) => { setModel(event.target.value); chooseModel(event.target.value); }}>
                {models.map((name) => <option key={name} value={name}>{name}</option>)}
              </select></label>}
          </div>
        </form>
      </section>
    </div>
  </Page>;
}
