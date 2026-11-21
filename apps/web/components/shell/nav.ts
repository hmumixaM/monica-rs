import {
  CalendarHeart, Contact, GitBranch, Inbox, Images, Lightbulb, Link2, type LucideIcon, MessageCircle, Newspaper, Plug, RefreshCw, ScanFace,
  Settings2, Sparkles, UserRoundPen,
} from "lucide-react";

export interface NavEntry {
  id: string; label: string; href: string; icon: LucideIcon; keywords?: string;
  /** Pages that share this entry. The sidebar shows the entry; its pages show these as tabs. */
  pages?: NavEntry[];
}
export interface NavGroup { id: string; label?: string; entries: NavEntry[] }

/** The index, grouped by what the user is doing: seeing people, understanding them,
 * tidying what was collected, and managing where it comes from. */
export const NAV_GROUPS: NavGroup[] = [
  { id: "today", entries: [
    { id: "dashboard", label: "今天", href: "/dashboard", icon: CalendarHeart, keywords: "home shouye jintian 首页" },
  ] },
  { id: "people", label: "人", entries: [
    { id: "profiles", label: "联系人", href: "/profiles", icon: Contact, keywords: "contacts lianxiren people" },
    { id: "messages", label: "聊天", href: "/messages", icon: MessageCircle, keywords: "messages chat liaotian" },
    { id: "timeline", label: "动态", href: "/timeline", icon: Images, keywords: "timeline feed dongtai pengyouquan 朋友圈" },
  ] },
  { id: "understand", label: "理解", entries: [
    { id: "ask", label: "问 Monica", href: "/ask", icon: Sparkles, keywords: "agent qa wen 人物问答" },
    { id: "insights", label: "洞察", href: "/insights", icon: Lightbulb, keywords: "insights stats dongcha 统计 日历", pages: [
      { id: "insights", label: "概览", href: "/insights", icon: Lightbulb },
      { id: "digest", label: "每周回顾", href: "/digest", icon: Newspaper, keywords: "digest weekly zhoubao 周报" },
      { id: "relationships", label: "关系与足迹", href: "/relationships", icon: GitBranch, keywords: "graph map orbit guanxi zuji 地图 关系图" },
    ] },
  ] },
  { id: "tidy", label: "整理", entries: [
    { id: "review", label: "待整理", href: "/suggestions", icon: Inbox, keywords: "review zhengli daizhengli", pages: [
      { id: "suggestions", label: "资料建议", href: "/suggestions", icon: UserRoundPen, keywords: "suggestions jianyi ai 生日 常驻地" },
      { id: "linking", label: "账号关联", href: "/linking", icon: Link2, keywords: "linking guanlian accounts" },
      { id: "faces", label: "照片里的人", href: "/faces", icon: ScanFace, keywords: "faces renlian photos 人脸" },
    ] },
  ] },
  { id: "data", label: "数据", entries: [
    { id: "sources", label: "来源", href: "/sources", icon: Plug, keywords: "sources laiyuan email 邮箱 同步 连接", pages: [
      { id: "sources", label: "连接", href: "/sources", icon: Plug },
      { id: "imports", label: "同步与导入", href: "/imports", icon: RefreshCw, keywords: "imports daoru tongbu 导入 进度" },
    ] },
    { id: "settings", label: "设置", href: "/settings", icon: Settings2, keywords: "settings shezhi theme 主题" },
  ] },
];

const ENTRIES = NAV_GROUPS.flatMap((group) => group.entries);

/** Every destination, with shared entries expanded into their pages: for the command palette and the phone's More sheet. */
export const ALL_PAGES: NavEntry[] = ENTRIES.flatMap((entry) => entry.pages
  ? entry.pages.map((page) => (page.id === entry.id ? { ...page, label: entry.label, keywords: entry.keywords } : page))
  : [entry]);

/** The sidebar entry a page belongs to. */
export const entryFor = (pageId: string) => ENTRIES.find((entry) => entry.id === pageId || entry.pages?.some((page) => page.id === pageId));

/** The phone tab bar: four destinations plus 更多. */
export const TAB_NAV = ["dashboard", "profiles", "messages", "ask"].map((id) => ENTRIES.find((entry) => entry.id === id)!);
