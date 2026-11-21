import type { AgentMessage } from '@earendil-works/pi-agent-core';

/** Rough token count: about one per CJK character, one per four of anything else. */
export function estimateTokens(value: unknown): number {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  const wide = text.match(/[\u2e80-\u9fff\uac00-\ud7af\uff00-\uffef]/g)?.length ?? 0;
  return wide + Math.ceil((text.length - wide) / 4);
}

/** A closed tab can leave assistant tool calls without results. The saved transcript keeps
 * them as they were; the next model context gets an explicit result for each. */
export function resumeContext(messages: AgentMessage[]): AgentMessage[] {
  const result: AgentMessage[] = [];
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i]; result.push(message);
    if (message.role !== 'assistant') continue;
    const replies = new Set<string>();
    let end = i + 1;
    while (end < messages.length && messages[end].role === 'toolResult') {
      const reply = messages[end];
      if (reply.role === 'toolResult') replies.add(reply.toolCallId);
      result.push(reply); end++;
    }
    for (const part of message.content) {
      if (part.type === 'toolCall' && !replies.has(part.id)) result.push({ role: 'toolResult',
        toolCallId: part.id, toolName: part.name, isError: true, timestamp: Date.now(),
        content: [{ type: 'text', text: '上次查询已中断，没有结果。需要时请重新查询。' }] });
    }
    i = end - 1;
  }
  return result;
}

const SHORTENED = '\n[这条查询结果在上下文里被截短了；需要其余部分时缩小范围重新查询。]';

function clip(message: AgentMessage, tokens: number): AgentMessage {
  if (message.role !== 'toolResult') return message;
  return { ...message, details: undefined, content: message.content.map(part => {
    if (part.type !== 'text' || estimateTokens(part.text) <= tokens) return part;
    // Cut by characters at the same ratio as the token estimate.
    const keep = Math.floor(part.text.length * tokens / estimateTokens(part.text));
    return { ...part, text: part.text.slice(0, keep) + SHORTENED };
  }) };
}

/** Fits the conversation into the model's context. The saved transcript is untouched: newest
 * turns are kept whole, older ones dropped, and only an oversized current turn loses its
 * earliest tool rounds. A tool call always stays with its results. */
export function compactContext(messages: AgentMessage[], contextWindow: number, outputTokens: number): AgentMessage[] {
  const budget = Math.max(2000, contextWindow - outputTokens - 3000);
  const turns: AgentMessage[][] = [];
  for (const message of messages) {
    if (message.role === 'user' || !turns.length) turns.push([]);
    turns.at(-1)!.push(message);
  }
  const selected: AgentMessage[][] = []; let used = 0;
  for (const [age, turn] of turns.toReversed().entries()) {
    // The current turn may spend most of the budget on what it just read; earlier turns keep a digest.
    const copy = turn.map(message => clip(message, age === 0 ? Math.floor(budget / 3) : 1500));
    const size = estimateTokens(copy);
    if (used + size <= budget) { selected.unshift(copy); used += size; continue; }
    if (selected.length) break;
    selected.unshift(newestRounds(copy, budget));
    break;
  }
  return selected.flat();
}

/** The question plus as many of the latest tool rounds as fit. */
function newestRounds(turn: AgentMessage[], budget: number): AgentMessage[] {
  const head = turn[0]?.role === 'user' ? [turn[0]] : [];
  const rounds: AgentMessage[][] = [];
  for (const message of turn.slice(head.length)) {
    if (message.role !== 'toolResult' || !rounds.length) rounds.push([]);
    rounds.at(-1)!.push(message);
  }
  let used = estimateTokens(head);
  if (used > budget / 2) throw new Error('这个问题太长，超出了模型的上下文。请分成几个问题来问。');
  const kept: AgentMessage[][] = [];
  for (const round of rounds.toReversed()) {
    const size = estimateTokens(round);
    if (used + size > budget) {
      if (!kept.length) throw new Error('这次查询的结果超出了模型的上下文。请停止后缩小查询范围。');
      break;
    }
    kept.unshift(round); used += size;
  }
  const first = head[0];
  if (first?.role === 'user' && kept.length < rounds.length) {
    const note = '\n[本轮较早的查询已移出上下文，需要时重新查询。]';
    head[0] = { ...first, content: typeof first.content === 'string' ? first.content + note : [...first.content, { type: 'text', text: note }] };
  }
  return [...head, ...kept.flat()];
}
