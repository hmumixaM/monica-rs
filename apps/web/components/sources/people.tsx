"use client";

import { useEffect, useState } from "react";

import { Button } from "../ui/button";
import { SearchInput } from "../ui/field";
import { PersonPicker } from "../ui/person-picker";
import { Failure, Faint } from "../ui/primitives";
import { message, request } from "./api";
import styles from "./sources.module.css";

type SourcePerson = {
  external_id: string; display_name: string; is_self: boolean; profile_id: string | null; profile_name: string | null; blocked: boolean | null;
  candidates: { id: string; name: string }[];
};
const PAGE = 40;

/** Who the people in one account are: matched automatically, or linked, unlinked and reset by hand. */
export function ConnectionPeople({ connection }: { connection: string }) {
  const [people, setPeople] = useState<SourcePerson[]>([]);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [more, setMore] = useState(false);
  const [linking, setLinking] = useState(false);
  const path = (after = "") => `/api/v1/source-connections/${connection}/people?` + new URLSearchParams({ q: query, after });
  async function load(append = false) {
    const rows = await request<SourcePerson[]>(path(append ? people.at(-1)?.external_id : ""));
    setPeople((old) => (append ? [...old, ...rows] : rows)); setMore(rows.length === PAGE);
  }
  useEffect(() => {
    let active = true;
    const timer = setTimeout(() => void request<SourcePerson[]>(path()).then((rows) => {
      if (active) { setPeople(rows); setMore(rows.length === PAGE); }
    }).catch((failure) => { if (active) setError(message(failure)); }), 200);
    return () => { active = false; clearTimeout(timer); };
  }, [connection, query]);
  async function decide(external_id: string, action: "link" | "unlink" | "automatic", profile_id?: string) {
    try { await request(`/api/v1/source-connections/${connection}/identity`, { external_id, action, profile_id }); setEditing(null); await load(); setError(""); }
    catch (failure) { setError(message(failure)); }
  }
  // People with exactly one same-name profile on the same platform can be linked in one step.
  const obvious = people.filter((person) => !person.profile_id && !person.blocked && person.candidates.length === 1);
  async function linkObvious() {
    setLinking(true);
    try {
      for (const person of obvious) await request(`/api/v1/source-connections/${connection}/identity`, { external_id: person.external_id, action: "link", profile_id: person.candidates[0].id });
      await load(); setError("");
    } catch (failure) { setError(message(failure)); } finally { setLinking(false); }
  }
  return <div className={styles.people}>
    {obvious.length > 1 && <Button size="sm" variant="highlight" disabled={linking} onClick={() => void linkObvious()}>
      {linking ? "正在关联…" : `关联这 ${obvious.length} 位同名联系人`}</Button>}
    <SearchInput aria-label="搜索来源人物" placeholder="搜索姓名或账号" value={query} onChange={(event) => setQuery(event.target.value)} />
    {error && <Failure>{error}</Failure>}
    <ul role="list">{people.map((person) => <li key={person.external_id}>
      <div>
        <strong>{person.display_name}{person.is_self && <small>我</small>}</strong>
        <span>{person.external_id}</span>
        <span>{person.blocked ? "已解除，不会自动关联" : person.profile_name ? `已关联到 ${person.profile_name}` : "等待自动匹配；有冲突时需要确认"}</span>
      </div>
      <div className={styles.actions}>
        {!person.profile_id && !person.blocked && person.candidates.map((candidate) =>
          <Button key={candidate.id} size="sm" variant="highlight" onClick={() => void decide(person.external_id, "link", candidate.id)}>关联到 {candidate.name}</Button>)}
        <Button size="sm" onClick={() => setEditing(editing === person.external_id ? null : person.external_id)}>选择人物</Button>
        {person.profile_id && <Button size="sm" variant="ghost" onClick={() => void decide(person.external_id, "unlink")}>解除关联</Button>}
        {person.blocked && <Button size="sm" variant="ghost" onClick={() => void decide(person.external_id, "automatic")}>恢复自动匹配</Button>}
      </div>
      {editing === person.external_id && <PersonPicker autoFocus onPick={(picked) => void decide(person.external_id, "link", picked.id)} />}
    </li>)}</ul>
    {!people.length && !error && <Faint>还没有收录到匹配的人物。</Faint>}
    {more && <Button size="sm" onClick={() => void load(true).catch((failure) => setError(message(failure)))}>更多人物</Button>}
  </div>;
}
