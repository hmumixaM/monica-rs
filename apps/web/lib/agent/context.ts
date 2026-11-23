import type { AgentMessage } from '@earendil-works/pi-agent-core';

const encoder = new TextEncoder();

/** Count dense text conservatively and account for the JSON transport size. */
export function estimateTokens(value: unknown): number {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  const wide = text.match(/[\u2e80-\u9fff\uac00-\ud7af\uff00-\uffef]/g)?.length ?? 0;
  return Math.max(wide + Math.ceil((text.length - wide) / 4), Math.ceil(encoder.encode(text).byteLength / 4));
}

export function contextBudget(contextWindow: number, outputTokens: number) {
  // Reserve room for the system prompt, tool definitions and the 2 MiB proxy body limit.
  return Math.min(350_000, Math.max(2_000, contextWindow - outputTokens - 3_000));
}

/** A closed tab can leave assistant tool calls without results. The saved transcript keeps
 * them as they were; the next model context gets an explicit result for each. */
export function resumeContext(messages: AgentMessage[]): AgentMessage[] {
  const result: AgentMessage[] = [];
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    // Each new turn has its own selected people, date, instructions and tool declarations.
    if (message.role === 'system') continue;
    if (message.role !== 'assistant') { result.push(message); continue; }
    let end = i + 1;
    while (messages[end]?.role === 'toolResult') end++;
    const toolResults = messages.slice(i + 1, end).filter(reply => reply.role === 'toolResult');
    i = end - 1;
    if (message.stopReason === 'error' || message.stopReason === 'aborted') continue;
    result.push(message, ...toolResults);
    const replies = new Set(toolResults.map(reply => reply.toolCallId));
    for (const part of message.content) {
      if (part.type === 'toolCall' && !replies.has(part.id)) result.push({ role: 'toolResult',
        toolCallId: part.id, toolName: part.name, isError: true, timestamp: Date.now(),
        content: [{ type: 'text', text: '上次查询已中断，没有结果。需要时请重新查询。' }] });
    }
  }
  return result;
}

function excerpt(text: string, tokens: number, note: string, keepEnding = false) {
  const size = estimateTokens(text);
  if (size <= tokens) return text;
  const keep = Math.max(0, Math.floor(text.length * (tokens - estimateTokens(note)) / size));
  const tail = keepEnding ? keep : Math.floor(keep / 4);
  return text.slice(0, keep - tail) + note + (tail ? text.slice(-tail) : '');
}

function clip(message: AgentMessage, tokens: number): AgentMessage {
  if (message.role !== 'toolResult') return message;
  return { ...message, details: undefined, content: message.content.map(part => part.type === 'text'
    ? { ...part, text: excerpt(part.text, tokens, '\n[查询结果已截短。可缩小范围再查。]\n') }
    : part) };
}

/** Fits the conversation into the model's context. The saved transcript is untouched: newest
 * turns are kept whole, older ones dropped, and only an oversized current turn loses its
 * earliest tool rounds. A tool call always stays with its results. */
export function compactContext(messages: AgentMessage[], budget: number): AgentMessage[] {
  const turns: AgentMessage[][] = [];
  for (const message of messages) {
    if (message.role === 'system') continue;
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
  return [...messages.filter(message => message.role === 'system'), ...selected.flat()];
}

/** The question plus as many of the latest tool rounds as fit. */
function newestRounds(turn: AgentMessage[], budget: number): AgentMessage[] {
  const question = turn[0]?.role === 'user' ? turn[0] : undefined;
  const head = question ? [question] : [];
  if (question && estimateTokens(question) > budget / 2) {
    const text = typeof question.content === 'string' ? question.content
      : question.content.flatMap(part => part.type === 'text' ? [part.text] : []).join('\n');
    head[0] = { ...question, content: excerpt(text, Math.floor(budget / 2) - 50, '\n[问题过长，中间部分已省略。]\n') };
  }
  const rounds: AgentMessage[][] = [];
  for (const message of turn.slice(head.length)) {
    if (message.role !== 'toolResult' || !rounds.length) rounds.push([]);
    rounds.at(-1)!.push(message);
  }
  let used = estimateTokens(head) + 64;
  const kept: AgentMessage[][] = [];
  let reduced = false;
  for (const round of rounds.toReversed()) {
    let copy = round;
    if (used + estimateTokens(copy) > budget) {
      const first = round[0];
      const room = budget - used;
      if (round.some(message => message.role === 'toolResult')) {
        // Share the remaining space without separating the calls and their results.
        copy = round.map(message => clip(message, Math.max(32, Math.floor(room / round.length) - 96)));
        reduced = true;
      } else if (first.role === 'assistant' && first.stopReason === 'length' && !first.content.some(part => part.type === 'toolCall')) {
        const text = first.content.flatMap(part => part.type === 'text' ? [part.text] : []).join('');
        const envelope = { ...first, content: [] };
        const tokens = room - estimateTokens(envelope) - 64;
        if (text.trim() && tokens > 96) {
          copy = [{ ...envelope, content: [{ type: 'text',
            text: excerpt(text, tokens, '[上一段回答前文已省略，以下为结尾。]\n', true) }] }];
          reduced = true;
        }
      }
    }
    const size = estimateTokens(copy);
    if (used + size > budget) break;
    kept.unshift(copy); used += size;
  }
  const first = head[0];
  if (first && (reduced || kept.length < rounds.length)) {
    const note = '\n[部分上下文已缩短。请根据保留的证据继续，需要时缩小范围再查。]';
    head[0] = { ...first, content: typeof first.content === 'string' ? first.content + note : [...first.content, { type: 'text', text: note }] };
  }
  return [...head, ...kept.flat()];
}
