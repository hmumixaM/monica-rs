import type { JsonObject } from '@earendil-works/pi-ai';
import type { RawDirectory } from '../api-types';
import {
  currentVerifiedDirectory, decodeVerifiedDirectory, directoryUrl,
  type Directory, type DirectoryFilters, type DirectorySort,
} from '../directory';
import { searchPeople } from '../search-client';
import { readWithRetry } from './requests';

export type CommandArgs = JsonObject;

function text(value: unknown, name: string) {
  if (value === undefined) return '';
  if (typeof value !== 'string') throw new Error(`${name} 必须是字符串。`);
  return value.trim();
}

function integer(value: unknown, fallback: number, minimum: number, maximum: number) {
  if (value === undefined) return fallback;
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`分页参数必须是 ${minimum} 到 ${maximum} 之间的整数。`);
  }
  return value;
}

function resolve(dictionary: Map<string, string>, value: unknown, kind: string) {
  const term = text(value, kind);
  if (!term) throw new Error(`请输入${kind}名称或 ID。`);
  if (dictionary.has(term)) return term;
  const hits = [...dictionary].filter(([, label]) => label.toLowerCase() === term.toLowerCase());
  if (hits.length !== 1) {
    const reason = hits.length ? '有同名选项，请用 ID' : '未找到';
    throw new Error(`${kind}「${term}」${reason}。可用 directory_facets 查看。`);
  }
  return hits[0][0];
}

const sorts: DirectorySort[] = ['relevance', 'name', 'last_contact', 'messages', 'birthday'];

/** The same snapshot, matching and ranking as the Contacts page. */
export function directoryCommands(signal?: AbortSignal) {
  let snapshot: Promise<Directory> | undefined;
  let lastSearch: { key: string; result: ReturnType<typeof searchPeople> } | undefined;
  function directory() {
    if (snapshot) return snapshot;
    const previous = currentVerifiedDirectory();
    const headers = previous ? { 'If-None-Match': previous.etag } : undefined;
    return snapshot = readWithRetry(directoryUrl, { signal, headers }, async response => {
      if (response.status === 304 && previous) return previous.directory;
      const raw = await response.json() as RawDirectory;
      return decodeVerifiedDirectory(raw, response.headers.get('ETag'));
    }).catch(error => {
      snapshot = undefined;
      throw error;
    });
  }
  return {
    async directory(args: CommandArgs) {
      const data = await directory();
      const type = text(args.type, 'type') || 'person';
      const sort = (text(args.sort, 'sort') || 'relevance') as DirectorySort;
      if (type !== 'person' && type !== 'group' && type !== 'all') throw new Error('type 可用 person、group 或 all。');
      if (!sorts.includes(sort)) throw new Error('未知排序字段。');
      const tags = args.tags ?? [];
      if (!Array.isArray(tags) || tags.length > 20) throw new Error('tags 必须是最多 20 项的名称或 ID 数组。');
      const filters: DirectoryFilters = {
        type: type === 'all' ? undefined : type,
        tagIds: tags.map(tag => resolve(data.tags, tag, '标签')).sort(),
        companyId: args.company === undefined ? undefined : resolve(data.companies, args.company, '公司'),
        schoolId: args.school === undefined ? undefined : resolve(data.schools, args.school, '学校'),
        provider: text(args.provider, 'provider') || undefined,
      };
      const offset = integer(args.offset, 0, 0, Number.MAX_SAFE_INTEGER);
      const limit = integer(args.limit, 100, 1, 500);
      const query = text(args.query, 'query');
      const key = JSON.stringify([query, filters, sort]);
      if (lastSearch?.key !== key) {
        const result = searchPeople(data, query, filters, sort, undefined, signal).catch(error => {
          if (lastSearch?.result === result) lastSearch = undefined;
          throw error;
        });
        lastSearch = { key, result };
      }
      const hits = await lastSearch.result;
      return {
        total: hits.length,
        next_offset: offset + limit < hits.length ? offset + limit : null,
        rows: hits.slice(offset, offset + limit).map(row => ({
          profile_id: row.id, name: row.name, type: row.type, city: row.city,
          summary: row.summary?.slice(0, 240), headline: row.headline?.slice(0, 240), providers: row.providers,
          tags: row.tagIds.map(id => data.tags.get(id)),
          companies: row.companyIds.map(id => data.companies.get(id)),
          schools: row.schoolIds.map(id => data.schools.get(id)),
          birthday: row.birthdayMd, last_contact: row.lastContactAt,
          messages: row.directMessageCount, members: row.memberCount, link: `/profiles/${row.id}`,
        })),
      };
    },
    async directory_facets(args: CommandArgs) {
      const data = await directory();
      const kind = text(args.kind, 'kind');
      const dictionary = new Map([['tags', data.tags], ['companies', data.companies], ['schools', data.schools]]).get(kind);
      if (!dictionary) throw new Error('kind 可用 tags、companies 或 schools。');
      const query = text(args.query, 'query').toLowerCase();
      const matches = [...dictionary].filter(([, name]) => name.toLowerCase().includes(query));
      const offset = integer(args.offset, 0, 0, Number.MAX_SAFE_INTEGER);
      const limit = integer(args.limit, 100, 1, 500);
      return {
        total: matches.length,
        next_offset: offset + limit < matches.length ? offset + limit : null,
        rows: matches.slice(offset, offset + limit).map(([id, name]) => ({ id, name })),
      };
    },
  };
}
