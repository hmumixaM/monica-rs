import { useQuery, useQueryClient } from "@tanstack/react-query";
import { BriefcaseBusiness, Camera, ContactRound, type LucideIcon, Mail, MessageCircle, MessagesSquare, Smartphone } from "lucide-react";

import { apiFetch } from "../../lib/api";

export type Connection = {
  id: string; provider: string; display_name: string; external_account: string; state: string; enabled: boolean; error: string | null;
  last_success_at: string | null; records?: number; earliest_at?: string; latest_at?: string; sync_pending?: boolean; pending_records?: number;
  progress?: string; coverage?: string;
};
export type SourceFile = { blob?: string; filename?: string; size?: number };
export type Evidence = {
  id: string; connection_id: string; external_id: string; title: string; body: string; raw_body?: string; category: string | null;
  occurred_at: string | null; account_name: string; provider: string; revision: number;
  versions?: { revision: number; recorded_at: string }[];
  events?: { kind: string; title: string; date_text?: string; organization?: string; location?: string; reference?: string; quote: string }[];
  copies?: { id: string; revision: number; account_name: string }[];
  metadata: { original_content_url?: string; original?: SourceFile; attachments?: SourceFile[]; participants?: { id: string; name?: string; role: string; is_self?: boolean }[] };
};
export type Capability = { id: string; collection_available: boolean; unavailable_reason?: string };

/** Reads JSON, or posts `body` as JSON; a failure carries the server's explanation. */
export async function request<T = any>(path: string, body?: unknown): Promise<T> {
  const response = await apiFetch(path, body ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) } : {});
  if (!response.ok) throw new Error((await response.json().catch(() => null))?.detail || `请求失败 (${response.status})`);
  return response.json();
}

export const PROVIDERS: { id: string; name: string; detail: string; icon: LucideIcon }[] = [
  { id: "mail", name: "邮件", detail: "Gmail、Outlook 与 IMAP 邮箱。只读同步，每个邮箱单独决定收录哪些邮件。", icon: Mail },
  { id: "wechat", name: "微信", detail: "联系人、聊天与朋友圈，来自 Chatlog 归档。", icon: MessagesSquare },
  { id: "whatsapp", name: "WhatsApp", detail: "连接设备同步新消息，也可以导入手机导出的历史。", icon: MessageCircle },
  { id: "linkedin", name: "LinkedIn", detail: "已授权归档的经历、资料、动态与消息。", icon: BriefcaseBusiness },
  { id: "instagram", name: "Instagram", detail: "已授权归档的个人资料、动态与快拍。", icon: Camera },
  { id: "apple_messages", name: "Messages", detail: "这台 Mac 上保存的短信与 iMessage。", icon: Smartphone },
  { id: "apple_contacts", name: "Contacts", detail: "Mac 通讯录里的姓名、电话与邮箱。", icon: ContactRound },
];

export const STATE_LABEL: Record<string, string> = {
  legacy: "已导入归档", connected: "已连接", connecting: "正在连接", paused: "已暂停", disconnected: "已断开", error: "需要处理",
  needs_auth: "需要重新授权", syncing: "同步中",
};
export const needsAttention = (connection: Connection) => connection.state === "error" || connection.state === "needs_auth";

const CONNECTIONS = ["source-connections"] as const;

/** Every connected account with its sync progress, refreshed while the page is open. */
export function useConnections() {
  return useQuery({
    queryKey: CONNECTIONS, refetchInterval: 10_000,
    queryFn: async () => {
      const [rows, progress] = await Promise.all([request<Connection[]>("/api/v1/source-connections"), request<Connection[]>("/api/v1/source-connections/progress")]);
      const byId = new Map(progress.map((row) => [row.id, row]));
      return rows.map((row) => ({ ...row, ...byId.get(row.id) }));
    },
  });
}
export function useRefreshConnections() {
  const client = useQueryClient();
  return () => client.invalidateQueries({ queryKey: CONNECTIONS });
}
export function useCapabilities() {
  return useQuery({
    queryKey: ["source-capabilities"], staleTime: 300_000,
    queryFn: async () => Object.fromEntries((await request<Capability[]>("/api/v1/sources")).map((row) => [row.id, row])) as Record<string, Capability>,
  });
}

export const CATEGORY_LABEL: Record<string, string> = { personal: "人际往来", transactional: "事务", marketing: "已过滤", pending: "待分类" };
export const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
