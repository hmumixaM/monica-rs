import type { AgentTool } from '@earendil-works/pi-agent-core';
import { apiFetch } from '../api';
import { messageText } from '../record-text';

/** What Monica can look up. Each tool reads the same endpoints as the pages do and returns
 * only what an answer needs: names, local times, text and a link back to the original. */

type Json = Record<string, any>;
type Args = Record<string, string | undefined>;

async function read(path: string, signal?: AbortSignal): Promise<any> {
  const response = await apiFetch(path, { signal });
  if (!response.ok) throw new Error(response.status === 503 ? '数据库正忙，请稍后重试这次查询' : `查询失败 (${response.status})`);
  return response.json();
}

const query = (values: Args) => new URLSearchParams(Object.entries(values).filter((entry): entry is [string, string] => !!entry[1])).toString();
const cut = (text: unknown, length: number) => { const value = String(text ?? '').trim(); return value.length > length ? value.slice(0, length) + '…' : value; };
const day = (value?: string | null) => value ? new Date(value).toLocaleDateString('sv-SE') : undefined;
const minute = (value?: string | null) => value ? new Date(value).toLocaleString('sv-SE').slice(0, 16) : undefined;
/** Model dates are local calendar days; the API takes instants. */
const instant = (value?: string) => value ? new Date(/^\d{4}-\d{2}-\d{2}$/.test(value) ? value + 'T00:00:00' : value).toISOString() : undefined;

function messageRow(row: Json) {
  return {
    at: minute(row.occurred_at),
    from: row.direction === 'sent' ? '我' : row.sender_name || 'TA',
    text: cut(messageText(String(row.text ?? '')), 600),
    // A hit from the search across all conversations says whose chat it is, so no second lookup is needed.
    ...(row.conversation_type === 'group' ? { group: row.conversation_name }
      : row.conversation_name ? { chat_with: row.conversation_name, profile_id: row.profile_id ?? undefined } : {}),
    link: row.evidence_id ? `/evidence/${row.evidence_id}?revision=${row.evidence_revision}` : `/messages?conversation=${row.conversation_id}&message=${row.id}`,
  };
}

/** Rows arrive newest first; `earlier` is the `before` value for the next page. */
function messagePage(rows: Json[], limit: number) {
  return { messages: rows.map(messageRow), earlier: rows.length === limit ? rows.at(-1)!.occurred_at : null };
}

async function person(id: string, signal?: AbortSignal) {
  const [overview, profile, ai] = await Promise.all([
    read(`/api/v1/profiles/${id}/overview`, signal), read(`/api/v1/profiles/${id}`, signal),
    read(`/api/v1/ai/profiles/${id}`, signal).catch(() => null),
  ]);
  const sources: Json = {};
  for (const [provider, fields] of Object.entries<Json>(profile.presentation?.sources ?? {})) {
    const kept = Object.entries(fields).filter(([key, value]) => typeof value === 'string' && value && value.length <= 400 && !/url|_id$/.test(key));
    if (kept.length) sources[provider] = Object.fromEntries(kept);
  }
  const facts = (profile.presentation?.facts ?? []).slice(0, 12).map((fact: Json) => {
    const value = fact.value ?? {};
    return cut([fact.fact_type === 'education' ? '教育' : '工作', fact.company_name || fact.school_name || value.company_name || value.school_name,
      value.title || value.degree, [value.start_date, value.end_date || (value.is_current ? '至今' : '')].filter(Boolean).join('—')].filter(Boolean).join(' · '), 200);
  });
  return {
    profile_id: id, name: overview.header.name, link: `/profiles/${id}`,
    note: overview.header.note || undefined, city: overview.header.city || undefined, summary: cut(overview.header.summary, 400) || undefined,
    tags: overview.tags?.map((tag: Json) => tag.name),
    chat: { total: overview.messages?.total, sent_by_me: overview.messages?.sent, received: overview.messages?.received,
      first: day(overview.messages?.first_contact_at), last: day(overview.messages?.last_contact_at) },
    memory: ai?.memory ? { summary: ai.memory.summary, facts: ai.memory.facts } : undefined,
    recent_weeks: ai?.weekly_updates?.filter((week: Json) => week.summary).slice(0, 4).map((week: Json) => ({ week: week.week_start, summary: week.summary })),
    career: facts.length ? facts : undefined,
    sources,
    relationships: overview.relationships?.slice(0, 20).map((row: Json) => `${row.relationship_type}：${row.name}`),
    groups: overview.groups?.slice(0, 15).map((group: Json) => group.name),
  };
}

function tool(name: string, label: string, description: string, properties: Record<string, { type: 'string'; description: string; enum?: string[] }>,
  required: string[], run: (args: Args, signal?: AbortSignal) => Promise<unknown>): AgentTool {
  return {
    name, label, description,
    parameters: { type: 'object', properties, required, additionalProperties: false } as unknown as AgentTool['parameters'],
    execute: async (_id, args, signal) => {
      const output = await run(args as Args, signal);
      return { content: [{ type: 'text', text: JSON.stringify(output) }], details: undefined };
    },
  };
}

const PERSON = { type: 'string', description: '人物的 profile_id' } as const;
const BEFORE = { type: 'string', description: '只看这个时间之前的内容，用于向更早翻页；填上一页返回的 earlier' } as const;

export const TOOL_LABELS: Record<string, string> = {
  find_people: '查找人物', get_person: '读取人物资料', search_messages: '搜索聊天', read_messages: '读取聊天',
  get_posts: '读取动态', search_records: '搜索动态与记录', search_mail: '搜索邮件与来源记录', read_record: '读取原文', relationship_overview: '查看往来概况',
};

export function tools(): AgentTool[] {
  return [
    tool('find_people', TOOL_LABELS.find_people, '按名字、备注或简介查找通讯录里的人，返回 profile_id。', {
      query: { type: 'string', description: '名字或关键词' },
    }, ['query'], async (a, signal) => (await read(`/api/v1/profiles?${query({ q: a.query, profile_type: 'person', limit: '10' })}`, signal))
      .map((row: Json) => ({ profile_id: row.id, name: row.display_name, summary: cut(row.summary, 120) || undefined }))),

    tool('get_person', TOOL_LABELS.get_person, '一个人的完整资料：备注、标签、各平台资料、工作教育经历、往来统计、Monica 整理的概括和最近几周的近况。了解一个人先用它。', {
      profile_id: PERSON,
    }, ['profile_id'], (a, signal) => person(a.profile_id!, signal)),

    tool('search_messages', TOOL_LABELS.search_messages, '按关键词搜索聊天记录，新的在前。不给 profile_id 时搜索所有私聊和群聊；给了就只搜这个人的私聊和 TA 在群里说的话。关键词按原文匹配，换同义词多搜几次。', {
      query: { type: 'string', description: '要匹配的原文片段，例如「面试」「offer」' },
      profile_id: { ...PERSON, description: '只搜这个人的聊天（可选）' },
      chat: { type: 'string', enum: ['direct', 'group'], description: '只搜私聊或群聊（可选）' },
      since: { type: 'string', description: '起始日期 YYYY-MM-DD（可选）' },
      before: BEFORE,
    }, ['query'], async (a, signal) => {
      const limit = 40;
      const rows = a.profile_id
        ? await read(`/api/v1/profiles/${a.profile_id}/messages?${query({ q: a.query, include_group_speech: 'true', conversation_type: a.chat, since: instant(a.since), before: instant(a.before), limit: String(limit) })}`, signal)
        : await read(`/api/v1/messages/search?${query({ q: a.query, conversation_type: a.chat, since: instant(a.since), before: instant(a.before), limit: String(limit) })}`, signal);
      return messagePage(rows, limit);
    }),

    tool('read_messages', TOOL_LABELS.read_messages, '按时间读取和一个人的聊天（含 TA 在群里说的话），新的在前。用 since、until 读某段时间，用 before 向更早翻页。', {
      profile_id: PERSON,
      since: { type: 'string', description: '起始日期 YYYY-MM-DD（可选）' },
      until: { type: 'string', description: '结束日期 YYYY-MM-DD，不含当天（可选）' },
      before: BEFORE,
    }, ['profile_id'], async (a, signal) => {
      const limit = 60;
      return messagePage(await read(`/api/v1/profiles/${a.profile_id}/messages?${query({ include_group_speech: 'true', since: instant(a.since), until: instant(a.until), before: instant(a.before), limit: String(limit) })}`, signal), limit);
    }),

    tool('get_posts', TOOL_LABELS.get_posts, '一个人发过的动态（朋友圈、Instagram、LinkedIn），新的在前，含图片内容的文字说明。', {
      profile_id: PERSON,
      cursor: { type: 'string', description: '上一页返回的 next，用于读更早的动态（可选）' },
    }, ['profile_id'], async (a, signal) => {
      const rows: Json[] = await read(`/api/v1/timeline?${query({ profile_id: a.profile_id, limit: '20', cursor: a.cursor })}`, signal);
      return { posts: rows.map(row => ({
        at: minute(row.occurred_at), provider: row.provider, text: cut(row.summary, 500),
        images: row.media?.map((media: Json) => media.description).filter(Boolean).slice(0, 4),
        place: row.location?.name || row.location?.poiName || undefined, likes: row.like_count || undefined, comments: row.comment_count || undefined,
        link: `/timeline?event=${row.id}`,
      })), next: rows.length === 20 ? rows.at(-1)!.cursor : null };
    }),

    tool('search_records', TOOL_LABELS.search_records, '在所有人的动态、笔记和人物资料里按关键词搜索。聊天记录用 search_messages。', {
      query: { type: 'string', description: '关键词' },
    }, ['query'], async (a, signal) => (await read(`/api/v1/search?${query({ q: a.query })}`, signal)).slice(0, 30).map((row: Json) => ({
      kind: row.kind, at: day(row.occurred_at), person: row.profile_name || undefined, profile_id: row.profile_id || undefined,
      title: cut(row.title, 80), text: cut(row.summary, 300),
      link: row.entity_type === 'timeline' ? `/timeline?event=${row.id}` : row.profile_id ? `/profiles/${row.profile_id}` : undefined,
    }))),

    tool('search_mail', TOOL_LABELS.search_mail, '搜索邮件和其他已连接来源（WhatsApp、短信、LinkedIn 消息）收录的记录。可按关键词、按人物，或两者一起。', {
      query: { type: 'string', description: '关键词（可选）' },
      profile_id: { ...PERSON, description: '只看与这个人有关的记录（可选）' },
      before: BEFORE,
    }, [], async (a, signal) => {
      const rows: Json[] = await read(`/api/v1/evidence?${query({ q: a.query, profile_id: a.profile_id, before: instant(a.before), limit: '20' })}`, signal);
      return { records: rows.map(row => ({
        record_id: row.id, at: minute(row.occurred_at), source: row.provider, account: row.account_name, title: cut(row.title, 120) || undefined,
        from: row.metadata?.participants?.filter((p: Json) => p.role === 'sender').map((p: Json) => p.is_self ? '我' : p.name || p.id).join('、') || undefined,
        text: cut(row.body, 400), link: `/evidence/${row.id}?revision=${row.revision}`,
      })), earlier: rows.length === 20 ? rows.at(-1)!.occurred_at : null };
    }),

    tool('read_record', TOOL_LABELS.read_record, '读取一条邮件或来源记录的完整正文。正文很长时用返回的 next_offset 继续读。', {
      record_id: { type: 'string', description: 'search_mail 返回的 record_id' },
      offset: { type: 'string', description: '从正文的这个位置继续读（可选）' },
    }, ['record_id'], async (a, signal) => {
      const row = await read(`/api/v1/evidence/${a.record_id}?${query({ body_limit: '8000', body_offset: a.offset || '0' })}`, signal);
      return { title: row.title, at: minute(row.occurred_at), source: row.provider, account: row.account_name, body: row.body,
        next_offset: row.next_body_offset ?? null, events: row.events?.length ? row.events : undefined, link: `/evidence/${row.id}?revision=${row.revision}` };
    }),

    tool('relationship_overview', TOOL_LABELS.relationship_overview, '整体往来概况：最近联系最多的人、很久没联系的老朋友、即将到来的生日和纪念日、最近发动态的人、最近一期每周回顾。回答「最近和谁联系多」「该联系谁了」这类问题用它。', {
    }, [], async (_a, signal) => {
      const [insights, home, digests] = await Promise.all([
        read('/api/v1/insights', signal), read('/api/v1/home', signal), read('/api/v1/digests', signal).catch(() => null),
      ]);
      const contact = (row: Json) => ({ profile_id: row.profile_id, name: row.name, last_contact: day(row.last_contact_at), direct_messages: row.direct_message_count,
        recent_months: row.months?.slice(-3).map((month: Json) => `${month.month} ${(month.sent ?? 0) + (month.received ?? 0)}条`) });
      return {
        totals: home.key_numbers,
        recent_contacts: insights.recent_contacts?.slice(0, 20).map(contact),
        dormant_contacts: insights.dormant_contacts?.slice(0, 15).map(contact),
        upcoming_dates: insights.upcoming_dates?.slice(0, 12).map((row: Json) => ({ profile_id: row.profile_id, name: row.name, kind: row.kind, label: row.label || undefined, date: row.date, days_until: row.days_until })),
        recent_posters: insights.recent_posters?.slice(0, 12).map((row: Json) => ({ profile_id: row.profile_id, name: row.name, posts: row.posts, latest: day(row.latest_at) })),
        latest_weekly_review: digests?.digests?.[0] ? { week: digests.digests[0].week_start, summary: digests.digests[0].summary } : undefined,
      };
    }),
  ];
}
