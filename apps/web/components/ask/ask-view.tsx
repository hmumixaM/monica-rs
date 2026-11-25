"use client";

import { useQueryClient } from "@tanstack/react-query";
import { ArrowUp, Brain, Check, ChevronRight, CircleAlert, Download, Ellipsis, MessageSquarePlus, Search, Sparkles, Square, Trash2, Upload, X } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";

import { useSearchParams } from "@/router/navigation";
import {
  activeConversation, chooseModel, cliTitle, createConversation, decideEdit, deleteConversation, editConversation,
  exportConversation, importConversation, importLegacy, legacyConversations, listConversations,
  modelChoices, run, selectConversation, stop, TOOL_LABELS,
  type CliQueryDetails, type Conversation, type Edit, type LegacyChat, type Message,
} from "../../lib/agent/sessions";
import { useConversation } from "../../lib/agent/use-conversation";
import { directoryKey } from "../../lib/directory";
import { fetchJson, queryKeys } from "../../lib/queries";
import { Button, IconButton } from "../ui/button";
import { Select } from "../ui/field";
import { Markdown } from "../ui/markdown";
import { Menu } from "../ui/menu";
import { PersonPicker, type PickedPerson } from "../ui/person-picker";
import { Avatar, Failure, Page, PageHeader, formatDateTime } from "../ui/primitives";
import styles from "./ask.module.css";

const STARTERS = ["我最近和谁联系得最多？", "有哪些很久没联系、值得问候的老朋友？", "接下来一个月有谁过生日？"];
const modelLabel = (name: string) => name.split("/").at(-1) === "deepseek-v4-pro-medium" ? "DeepSeek V4 Pro · Medium" : name;
const PERSON_STARTERS = ["TA 最近在忙什么？", "我们上次聊了什么？", "我答应过 TA 什么还没做的事？"];

type Step = { id: string; label: string; detail: string; script?: string; queries?: CliQueryDetails; failed: boolean; done: boolean };
type Turn = { question: string; steps: Step[]; editIds: string[]; thinking: string[]; answer: string };

/** A question, what Monica looked up for it, and what it said. */
function turns(messages: Message[]): Turn[] {
  const result: Turn[] = [];
  const steps = new Map<string, Step>();
  for (const message of messages) {
    if (message.role === "user") {
      steps.clear();
      result.push({ question: message.content, steps: [], editIds: [], thinking: [], answer: "" });
      continue;
    }
    const turn = result.at(-1);
    if (!turn) continue;
    if (message.role === "toolResult") {
      if (message.toolName === "propose_monica_edit") {
        const detail = message.details as { editId?: unknown } | undefined;
        if (typeof detail?.editId === "string" && !turn.editIds.includes(detail.editId)) turn.editIds.push(detail.editId);
        continue;
      }
      const step = steps.get(message.toolCallId);
      if (step) {
        step.done = true;
        step.failed = message.isError;
        const details = message.details as Partial<CliQueryDetails> | undefined;
        if (message.toolName === "monica_cli" && Array.isArray(details?.queries)) {
          step.queries = {
            queries: details.queries.filter(query => typeof query === "string"),
            omittedQueries: Number.isSafeInteger(details.omittedQueries) ? details.omittedQueries! : 0,
          };
        }
      }
    } else if (message.role === "assistant") {
      for (const part of message.content) {
        if (part.type === "text") turn.answer += part.text;
        if (part.type === "thinking" && !part.redacted && typeof part.thinking === "string" && part.thinking.trim()) {
          turn.thinking.push(part.thinking);
        }
        if (part.type === "toolCall" && message.stopReason !== "error" && message.stopReason !== "aborted") {
          if (part.name === "propose_monica_edit") continue;
          const args = part.arguments as Record<string, unknown>;
          const script = part.name === "monica_cli" && typeof args.script === "string" ? args.script : undefined;
          const label = part.name === "monica_cli" ? cliTitle(args.title) : TOOL_LABELS[part.name] ?? part.name;
          const step = { id: `${turn.steps.length}-${part.id}`, label, detail: String(args.query ?? args.since ?? ""), script, failed: false, done: false };
          steps.set(part.id, step); turn.steps.push(step);
        }
      }
    }
  }
  return result;
}

function Thinking({ parts }: { parts: string[] }) {
  const [expanded, setExpanded] = useState(false);
  return <details className={styles.steps} onToggle={event => setExpanded(event.currentTarget.open)}>
    <summary><Brain size={13} aria-hidden="true" />模型思考</summary>
    {expanded && <pre className={styles.thinking} tabIndex={0} aria-label="模型思考详情" aria-live="off">
      {parts.join("\n\n")}
    </pre>}
  </details>;
}

function Steps({ steps, live }: { steps: Step[]; live: boolean }) {
  if (!steps.length) return null;
  return <details className={styles.steps} open={live || undefined}>
    <summary><Search size={13} aria-hidden="true" />查询记录（{steps.length} 步）</summary>
    <ol>{steps.map((step) => <li key={step.id} data-failed={step.failed ? "" : undefined} data-pending={!step.done ? "" : undefined}>
      {step.failed ? <CircleAlert size={13} aria-hidden="true" /> : <Check size={13} aria-hidden="true" />}
      {step.script === undefined ? <>{step.label}{step.detail && <q>{step.detail}</q>}</> :
        <details className={styles.queryScript}>
          <summary><ChevronRight size={12} aria-hidden="true" />{step.label}</summary>
          <pre aria-label="JavaScript 脚本"><code>{step.script}</code></pre>
          {step.queries && (step.queries.queries.length > 0 || step.queries.omittedQueries > 0) && <>
            <p>查询命令与参数{step.queries.omittedQueries > 0 && `（显示 ${step.queries.queries.length} 条，共 ${step.queries.queries.length + step.queries.omittedQueries} 条）`}</p>
            <pre aria-label="查询命令与参数"><code>{step.queries.queries.join("\n")}</code></pre>
          </>}
        </details>}
      {step.failed && <span>查询失败</span>}
      {!step.done && !live && <span>查询未完成</span>}
    </li>)}</ol>
  </details>;
}

function EditFields({ fields }: { fields: Record<string, unknown> }) {
  const entries = Object.entries(fields).filter(([, value]) => value !== null && value !== undefined
    && (!Array.isArray(value) || value.length > 0));
  if (!entries.length) return <p className={styles.editEmpty}>无记录</p>;
  return <dl className={styles.editFields}>{entries.map(([label, value]) => <div key={label}>
    <dt>{label}</dt>
    <dd>{typeof value === "boolean" ? (value ? "是" : "否") : Array.isArray(value) ? value.join("、") : label === "发生时间" && typeof value === "string" ? formatDateTime(value) : String(value)}</dd>
  </div>)}</dl>;
}

function EditCard({ chat, edit, refresh }: { chat: string; edit: Edit; refresh: () => void }) {
  const client = useQueryClient();
  const [submitting, setSubmitting] = useState(false);
  const [failure, setFailure] = useState("");
  const [receipt, setReceipt] = useState<Edit | null>(null);
  const current = edit.status === "pending" && receipt ? receipt : edit;
  const pending = current.status === "pending";
  const status = { pending: "待确认", applied: "已应用", dismissed: "已取消", stale: "预览已过期" }[current.status];

  async function decide(confirm: boolean) {
    setSubmitting(true);
    setFailure("");
    try {
      const saved = await decideEdit(chat, current.id, confirm);
      setReceipt(saved);
      if (saved.status === "applied") {
        for (const person of saved.preview.people) {
          void client.invalidateQueries({ queryKey: queryKeys.profile(person.id) });
        }
        for (const key of [directoryKey, queryKeys.home, ["tags"], ["timeline"], ["insights"]]) {
          void client.invalidateQueries({ queryKey: key });
        }
      }
      refresh();
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  }

  return <section className={styles.editCard} aria-label={current.preview.title}>
    <header><strong>{current.preview.title}</strong><span role="status">{status}</span></header>
    {current.preview.people.length > 0 && <details className={styles.editPeople} open={pending || undefined}>
      <summary>涉及 {current.preview.people.length} 位联系人</summary>
      <ul>{current.preview.people.map(person => <li key={person.id}><a href={`/profiles/${person.id}`}>{person.name}</a></li>)}</ul>
    </details>}
    {pending ? <>
      {current.preview.before && <><p className={styles.editLabel}>当前</p><EditFields fields={current.preview.before} /></>}
      <p className={styles.editLabel}>拟写入</p>
      <EditFields fields={current.preview.after} />
      <div className={styles.editActions}>
        <Button variant="primary" disabled={submitting} onClick={() => void decide(true)}>确认写入</Button>
        <Button variant="secondary" disabled={submitting} onClick={() => void decide(false)}>取消</Button>
      </div>
    </> : <details className={styles.editPeople}>
      <summary>查看编辑内容</summary>
      <EditFields fields={current.preview.after} />
    </details>}
    {current.result?.message && <p className={styles.editEmpty}>{current.result.message}</p>}
    {current.result?.link && <a href={current.result.link}>查看记录</a>}
    {failure && <Failure>{failure}</Failure>}
  </section>;
}

export function AskView() {
  const params = useSearchParams();
  const [id, setId] = useState("");
  const [sessions, setSessions] = useState<Conversation[]>([]);
  const [ready, setReady] = useState(false);
  const [people, setPeople] = useState<PickedPerson[]>([]);
  const [question, setQuestion] = useState("");
  const [failure, setFailure] = useState("");
  const [picking, setPicking] = useState(false);
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("");
  const [legacy, setLegacy] = useState<LegacyChat[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [savingPeople, setSavingPeople] = useState(false);
  const pending = useRef<{ id: string; question: string; request: string } | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const state = useConversation(id);
  const running = state.run?.status === "queued" || state.run?.status === "running";
  const transcript = useMemo(() => turns(state.run?.draft ? [...state.messages, state.run.draft] : state.messages),
    [state.messages, state.run?.draft]);
  const locked = !ready || submitting || savingPeople;
  const busy = locked || running || (!!id && !state.loaded);
  const report = (error: unknown) => setFailure(error instanceof Error ? error.message : String(error));

  function open(session?: Conversation) {
    const id = session?.id ?? "";
    pending.current = null;
    selectConversation(id);
    setId(id);
    setPeople(session?.people ?? []);
    setQuestion("");
    setFailure("");
  }
  useEffect(() => {
    const controller = new AbortController();
    void modelChoices().then(value => { setModels(value.models); setModel(value.chosen); }).catch(() => undefined);
    void legacyConversations().then(setLegacy).catch(() => undefined);
    async function load() {
      const rows = await listConversations(controller.signal, true);
      if (controller.signal.aborted) return;
      setSessions(rows);
      open(rows.find(row => row.id === activeConversation()) ?? rows[0]);
      setReady(true);
    }
    void load().catch(error => { if (!controller.signal.aborted) { report(error); setReady(true); } });
    return () => controller.abort();
  }, []);
  useEffect(() => {
    if (!ready) return;
    const controller = new AbortController();
    let pending = false;
    async function refreshSessions() {
      if (pending) return;
      pending = true;
      try {
        const rows = await listConversations(controller.signal);
        if (!controller.signal.aborted) setSessions(rows);
      } catch { /* The active conversation handles connection status. */ }
      finally { pending = false; }
    }
    void refreshSessions();
    const interval = setInterval(refreshSessions, 3500);
    return () => { controller.abort(); clearInterval(interval); };
  }, [ready, id, running]);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [state.messages, state.run?.draft, state.run?.progress]);

  async function choosePeople(next: PickedPerson[]) {
    setPeople(next);
    if (!id) return;
    setSavingPeople(true);
    try {
      const session = await editConversation(id, next);
      setSessions(rows => rows.map(row => row.id === id ? session : row));
    } finally {
      setSavingPeople(false);
    }
  }
  // Arriving from a profile ("问 Monica 关于 TA") starts with that person selected.
  const fromProfile = params.get("profile");
  const applied = useRef<string | null>(null);
  useEffect(() => {
    if (!fromProfile || !ready || applied.current === fromProfile) return;
    let active = true;
    void fetchJson<{ id: string; display_name: string; avatar_media_id?: string | null }>(`/api/v1/profiles/${encodeURIComponent(fromProfile)}`)
      .then(profile => {
        if (!active) return;
        applied.current = fromProfile;
        return choosePeople([{ id: profile.id, display_name: profile.display_name, avatar_media_id: profile.avatar_media_id ?? null }]);
      })
      .catch(error => { if (active) report(error); });
    return () => { active = false; };
  }, [fromProfile, ready, id]);

  // A question typed on the home page arrives ready to send.
  const handedOver = params.get("q");
  const appliedQuestion = useRef<string | null>(null);
  useEffect(() => {
    if (!handedOver || !ready || appliedQuestion.current === handedOver) return;
    appliedQuestion.current = handedOver;
    setQuestion(handedOver); input.current?.focus();
  }, [handedOver, ready]);

  function startNew() {
    open();
    input.current?.focus();
  }
  async function remove(session: Conversation) {
    try {
      await deleteConversation(session.id);
      const rest = await listConversations();
      setSessions(rest);
      if (session.id === id) open(rest[0]);
    } catch (error) { report(error); }
  }
  async function importFile(file: File) {
    try {
      const session = await importConversation(file);
      setSessions(rows => [session, ...rows]);
      open(session);
    } catch (error) { report(error); }
  }
  async function importBrowserHistory() {
    setSubmitting(true);
    try {
      for (const chat of legacy) {
        const session = await importLegacy(chat);
        setSessions(rows => [session, ...rows]);
        setLegacy(rows => rows.filter(row => row.localId !== chat.localId));
        open(session);
      }
    } catch (error) { report(error); }
    finally { setSubmitting(false); }
  }
  async function ask() {
    const value = question.trim();
    if (!value || busy) return;
    setFailure("");
    setSubmitting(true);
    try {
      const attempt = pending.current;
      const chat = id || (attempt?.question === value ? attempt.id : crypto.randomUUID());
      const requestId = attempt?.id === chat && attempt.question === value ? attempt.request : crypto.randomUUID();
      pending.current = { id: chat, question: value, request: requestId };
      if (!id) {
        const created = await createConversation(chat);
        setId(chat);
        selectConversation(chat);
        setSessions(rows => [created, ...rows.filter(row => row.id !== chat)]);
      }
      await run(chat, value, people, requestId);
      pending.current = null;
      setQuestion("");
      state.refresh();
      setSessions(await listConversations());
    } catch (error) { report(error); }
    finally { setSubmitting(false); }
  }
  function onKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); void ask(); }
  }
  const fill = (text: string) => { setQuestion(text); input.current?.focus(); };
  const current = sessions.find((session) => session.id === id);

  return <Page width="wide" className={styles.page}>
    <PageHeader title="问 Monica" description="问一个人的近况、聊过的事，或者整体的往来。回答里的链接可以打开原文。" />
    <div className={styles.workspace}>
      <aside className={styles.sessions} aria-label="会话">
        <Button block variant="secondary" disabled={locked} onClick={startNew}><MessageSquarePlus size={15} aria-hidden="true" />新会话</Button>
        <ul role="list">{sessions.map((session) => <li key={session.id} data-current={session.id === id ? "" : undefined}>
          <button type="button" disabled={locked} aria-current={session.id === id ? "true" : undefined}
            onClick={() => { if (session.id !== id) open(session); }}>{session.title}</button>
          <IconButton size="sm" label={`删除会话：${session.title}`} disabled={locked}
            onClick={() => void remove(session)}><Trash2 size={13} /></IconButton>
        </li>)}</ul>
      </aside>

      <section className={styles.chat} aria-label="当前会话">
        <header className={styles.chatHeader}>
          <Select className={styles.sessionSelect} aria-label="选择会话" value={id} disabled={locked} onChange={(event) => {
            const selected = sessions.find((session) => session.id === event.target.value);
            open(selected);
          }}>{!id && <option value="">新会话</option>}{sessions.map((session) => <option key={session.id} value={session.id}>{session.title}</option>)}</Select>
          <h2>{current?.title ?? "新会话"}</h2>
          <IconButton className={styles.newMobile} label="新会话" disabled={locked} onClick={startNew}><MessageSquarePlus size={17} /></IconButton>
          <Menu trigger={<IconButton label="会话操作" disabled={locked}><Ellipsis size={17} /></IconButton>} items={[
            ...(current ? [{ label: "删除当前会话", icon: <Trash2 size={15} />, onSelect: () => void remove(current) }] : []),
            { label: "导出会话", disabled: !id, icon: <Download size={15} />, onSelect: () => void exportConversation(id).catch(report) },
            { label: "导入会话", icon: <Upload size={15} />, disabled: submitting, onSelect: () => fileInput.current?.click() },
            ...(legacy.length ? [{ label: `导入此浏览器的旧会话（${legacy.length}）`, icon: <Upload size={15} />, disabled: submitting,
              onSelect: () => void importBrowserHistory() }] : []),
          ]} />
          <input ref={fileInput} hidden type="file" aria-label="导入会话文件" accept="application/json" onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void importFile(file);
            event.target.value = "";
          }} />
        </header>

        <div className={styles.transcript} aria-live="polite">
          {id && !state.loaded && !state.failure && <p className={styles.status} role="status"><span aria-hidden="true" />正在加载会话…</p>}
          {!transcript.length && (!id || state.loaded) && <div className={styles.welcome}>
            <Sparkles size={22} aria-hidden="true" />
            <strong>{people.length ? `想了解${people.map((person) => person.display_name).join("、")}的什么？` : "想知道什么？"}</strong>
            <p>{people.length ? "Monica 会翻看聊天、动态和已整理的资料来回答。" : "直接提问，或者先在下面选一个人，问题就会围绕 TA。"}</p>
            <div className={styles.starters}>{(people.length ? PERSON_STARTERS : STARTERS).map((text) =>
              <button key={text} type="button" disabled={locked} onClick={() => fill(text)}>{text}</button>)}</div>
          </div>}
          {transcript.map((turn, index) => {
            const live = running && index === transcript.length - 1;
            return <article key={index} className={styles.turn}>
              <p className={styles.question}>{turn.question}</p>
              <div className={styles.answer}>
                <Steps steps={turn.steps} live={live} />
                {turn.editIds.map(editId => {
                  const edit = state.edits.find(row => row.id === editId);
                  return edit && <EditCard key={edit.id} chat={id} edit={edit} refresh={state.refresh} />;
                })}
                {turn.thinking.length > 0 && <Thinking key={id} parts={turn.thinking} />}
                {turn.answer.trim() && <Markdown>{turn.answer}</Markdown>}
                {live && <p className={styles.status} role="status"><span aria-hidden="true" />{state.run?.progress}</p>}
              </div>
            </article>;
          })}
          {(failure || state.failure || state.run?.error) && <Failure>{failure || state.failure || state.run?.error}</Failure>}
          <div ref={end} />
        </div>

        <form className={styles.composer} onSubmit={(event) => { event.preventDefault(); void ask(); }}>
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
            <textarea ref={input} id="monica-question" name="question" aria-label="问题" rows={2} value={question} disabled={locked} onKeyDown={onKey}
              onChange={(event) => setQuestion(event.target.value)} placeholder={people.length ? "问关于 TA 的问题…" : "输入问题…"} />
            {running && state.run
              ? <IconButton label="停止回答" className={styles.send} onClick={() => void stop(id, state.run!.id).then(state.refresh).catch(report)}><Square size={15} /></IconButton>
              : <button type="submit" className={styles.send} aria-label="提问" title="提问（Enter）" disabled={busy || !question.trim()}><ArrowUp size={17} /></button>}
          </div>
          <div className={styles.footnote}>
            <small>Enter 发送，Shift + Enter 换行。会话自动保存，离开页面后仍会继续回答。</small>
            {models.length > 1 && <label className={styles.model}>模型
              <select aria-label="回答用的模型" value={running ? state.run?.model ?? model : model} disabled={running || submitting}
                onChange={(event) => { setModel(event.target.value); chooseModel(event.target.value); }}>
                {models.map((name) => <option key={name} value={name}>{modelLabel(name)}</option>)}
              </select></label>}
          </div>
        </form>
      </section>
    </div>
  </Page>;
}
