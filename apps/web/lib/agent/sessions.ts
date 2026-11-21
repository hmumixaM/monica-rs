"use client";
import { get, set, update } from 'idb-keyval';
import type { PickedPerson } from '../../components/ui/person-picker';

export type Conversation = { id: string; title: string; people: PickedPerson[]; updatedAt: number };
const indexKey = 'monica:ask:sessions';
const activeKey = 'monica:ask:active';

export async function listConversations(): Promise<Conversation[]> {
  const saved = await get<Conversation[]>(indexKey);
  if (saved) return saved.sort((a, b) => b.updatedAt - a.updatedAt);
  // The original single-session transcript remains at its existing key.
  const initial = [{ id: 'default', title: '新会话', people: [], updatedAt: Date.now() }];
  await update<Conversation[]>(indexKey, current => current ?? initial);
  return (await get<Conversation[]>(indexKey))!;
}

export async function saveConversation(value: Conversation) {
  await update<Conversation[]>(indexKey, current => [value, ...(current ?? []).filter(c => c.id !== value.id)]);
}

export async function createConversation() {
  const value: Conversation = { id: crypto.randomUUID(), title: '新会话', people: [], updatedAt: Date.now() };
  await saveConversation(value);
  await set(activeKey, value.id);
  return value;
}

export async function deleteConversation(id: string) {
  await update<Conversation[]>(indexKey, current => (current ?? []).filter(c => c.id !== id));
}

export const activeConversation = () => get<string>(activeKey);
export const selectConversation = (id: string) => set(activeKey, id);
