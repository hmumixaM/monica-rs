import {
  Database, GitBranch, House, Images, Lightbulb, Link2, type LucideIcon, MessageCircle, Newspaper, ScanFace, Settings2, Sparkles, Users,
} from "lucide-react";

export interface NavEntry { id: string; label: string; href: string; icon: LucideIcon; keywords?: string }

/** Everyday destinations, including first-class source connections. */
export const PRIMARY_NAV: NavEntry[] = [
  { id: "dashboard", label: "首页", href: "/dashboard", icon: House, keywords: "home shouye" },
  { id: "timeline", label: "动态", href: "/timeline", icon: Images, keywords: "timeline feed dongtai pengyouquan 朋友圈" },
  { id: "profiles", label: "联系人", href: "/profiles", icon: Users, keywords: "contacts lianxiren people" },
  { id: "messages", label: "聊天", href: "/messages", icon: MessageCircle, keywords: "messages chat liaotian" },
  { id: "sources", label: "来源", href: "/sources", icon: Database, keywords: "sources email 邮箱 同步" },
  { id: "ask", label: "问 Monica", href: "/ask", icon: Sparkles, keywords: "agent qa 人物问答" },
  { id: "relationships", label: "关系与足迹", href: "/relationships", icon: GitBranch, keywords: "graph map orbit guanxi zuji 地图 关系图" },
  { id: "insights", label: "洞察", href: "/insights", icon: Lightbulb, keywords: "insights stats dongcha 统计 日历" },
];

/** Data management, kept at the bottom of the sidebar and in the phone's More sheet. */
export const DATA_NAV: NavEntry[] = [
  { id: "suggestions", label: "资料建议", href: "/suggestions", icon: Sparkles, keywords: "suggestions jianyi ai 生日 常驻地" },
  { id: "linking", label: "账号关联", href: "/linking", icon: Link2, keywords: "linking guanlian accounts" },
  { id: "imports", label: "导入", href: "/imports", icon: Database, keywords: "imports daoru" },
  { id: "faces", label: "人脸", href: "/faces", icon: ScanFace, keywords: "faces renlian photos" },
  { id: "settings", label: "设置", href: "/settings", icon: Settings2, keywords: "settings shezhi theme 主题" },
];

export const DIGEST_NAV: NavEntry = { id: "digest", label: "周报", href: "/digest", icon: Newspaper, keywords: "digest weekly zhoubao" };

/** The phone tab bar: four destinations plus 更多. */
export const TAB_NAV = ["dashboard", "profiles", "messages", "sources"].map(id => PRIMARY_NAV.find(entry => entry.id === id)!);

/** Pages that light up a primary entry even though they have their own id. */
export const ACTIVE_ALIAS: Record<string, string> = { search: "", digest: "insights" };
