"use client";

import { useQueryClient } from "@tanstack/react-query";
import { LogOut, Monitor, Moon, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Search, Sun } from "lucide-react";
import Link from "@/router/link";
import { useRouter } from "@/router/navigation";
import { Fragment, type ReactNode, useCallback, useEffect, useState } from "react";

import { apiFetch } from "../lib/api";
import { clearClientData } from "../lib/queries";
import { forgetRecent, type RecentPerson, useRecentPeople } from "../lib/recent";
import { cycleTheme, SIDEBAR_KEY, type ThemeChoice, useThemeChoice } from "../lib/theme";
import styles from "./shell/shell.module.css";
import { CommandPalette } from "./shell/command-palette";
import { HubContext } from "./shell/hub";
import { ALL_PAGES, entryFor, NAV_GROUPS, type NavEntry, TAB_NAV } from "./shell/nav";
import { IconButton, Kbd } from "./ui/button";
import { Sheet } from "./ui/dialog";
import { Avatar } from "./ui/primitives";
import { PersonLink } from "./ui/person-link";

const THEME_LABEL: Record<ThemeChoice, string> = { system: "跟随系统", light: "浅色", dark: "深色" };
const THEME_ICON: Record<ThemeChoice, ReactNode> = { system: <Monitor size={17} />, light: <Sun size={17} />, dark: <Moon size={17} /> };

function typingTarget(target: EventTarget | null) {
  const element = target as HTMLElement | null;
  return !!element && (element.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(element.tagName));
}

export function AppShell({ active, children }: { active: string; children: ReactNode }) {
  const router = useRouter();
  const client = useQueryClient();
  const recent = useRecentPeople();
  const theme = useThemeChoice();
  const [palette, setPalette] = useState(false);
  const [more, setMore] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const entry = entryFor(active);
  const current = entry?.id ?? "";

  useEffect(() => {
    setCollapsed(document.documentElement.dataset.sidebar === "collapsed");
    fetch("/api/v1/me").then((response) => {
      if (response.status === 401) window.location.assign("/login");
    }).catch(() => undefined);
  }, []);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPalette((open) => !open);
        return;
      }
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey || typingTarget(event.target)) return;
      event.preventDefault();
      // A page with its own filter box (contacts) claims "/" for it.
      const local = document.querySelector<HTMLInputElement>("[data-slash-target]");
      if (local) local.focus();
      else setPalette(true);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const toggleSidebar = useCallback(() => {
    setCollapsed((value) => {
      const next = !value;
      if (next) document.documentElement.dataset.sidebar = "collapsed";
      else delete document.documentElement.dataset.sidebar;
      localStorage.setItem(SIDEBAR_KEY, next ? "collapsed" : "open");
      return next;
    });
  }, []);

  async function logout() {
    await apiFetch("/api/v1/auth/logout", { method: "POST" });
    await clearClientData(client);
    forgetRecent();
    router.replace("/login");
  }

  const navLink = (item: NavEntry) => <Link key={item.id} href={item.href} className={styles.navItem}
    aria-current={current === item.id ? "page" : undefined} title={collapsed ? item.label : undefined}>
    <item.icon size={18} strokeWidth={1.7} aria-hidden="true" /><span>{item.label}</span>
  </Link>;
  const closeOnLink = (close: () => void) => (event: React.MouseEvent) => { if ((event.target as HTMLElement).closest("a")) close(); };

  return <div className={styles.frame}>
    <a className={styles.skip} href="#main-content">跳转到主要内容</a>
    <aside className={styles.sidebar} aria-label="主导航">
      <div className={styles.brandRow}>
        <Link className={styles.brand} href="/dashboard" aria-label="Monica 首页"><span>M</span><strong>Monica</strong></Link>
        <IconButton label={collapsed ? "展开侧栏" : "收起侧栏"} size="sm" onClick={toggleSidebar} className={styles.collapse}>
          {collapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
        </IconButton>
      </div>
      <button type="button" className={styles.searchButton} onClick={() => setPalette(true)} aria-label="搜索（⌘K）">
        <Search size={16} aria-hidden="true" /><span>搜索</span><span className={styles.shortcut}><Kbd>⌘</Kbd><Kbd>K</Kbd></span>
      </button>
      <nav className={styles.nav}>
        {NAV_GROUPS.map((group) => <Fragment key={group.id}>
          {group.label && <h2>{group.label}</h2>}
          {group.entries.map(navLink)}
        </Fragment>)}
      </nav>
      {recent.length > 0 && <section className={styles.recent} aria-labelledby="recent-heading">
        <h2 id="recent-heading">最近查看</h2>
        <RecentList people={recent} />
      </section>}
      <div className={styles.utility}>
        <button type="button" className={styles.navItem} onClick={cycleTheme} title={`主题：${THEME_LABEL[theme]}`}>
          {THEME_ICON[theme]}<span>主题：{THEME_LABEL[theme]}</span>
        </button>
        <button type="button" className={styles.navItem} onClick={() => void logout()} title="退出">
          <LogOut size={17} strokeWidth={1.7} aria-hidden="true" /><span>退出</span>
        </button>
      </div>
    </aside>

    <header className={styles.mobileBar} data-mobile-bar="">
      <Link className={styles.brand} href="/dashboard" aria-label="Monica 首页"><span>M</span></Link>
      <button type="button" className={styles.mobileSearch} onClick={() => setPalette(true)}>
        <Search size={16} aria-hidden="true" /><span>找人、动态或页面</span>
      </button>
    </header>

    <main id="main-content" className={styles.main}>
      <HubContext.Provider value={entry?.pages ? { pages: entry.pages, active } : null}>{children}</HubContext.Provider>
    </main>

    <nav className={styles.tabbar} aria-label="主导航">
      {TAB_NAV.map((item) => <Link key={item.id} href={item.href} aria-current={current === item.id ? "page" : undefined}>
        <item.icon size={21} strokeWidth={1.7} aria-hidden="true" /><span>{item.label}</span>
      </Link>)}
      <button type="button" onClick={() => setMore(true)} aria-current={TAB_NAV.some((item) => item.id === current) ? undefined : "page"}
        aria-haspopup="dialog">
        <MoreHorizontal size={21} strokeWidth={1.7} aria-hidden="true" /><span>更多</span>
      </button>
    </nav>

    {more && <Sheet label="更多" onClose={() => setMore(false)}>
      <div className={styles.moreGrid} onClick={closeOnLink(() => setMore(false))}>
        {ALL_PAGES.filter((page) => !TAB_NAV.some((tab) => tab.id === page.id)).map((page) => <Link key={page.id} href={page.href}
          aria-current={active === page.id ? "page" : undefined}>
          <page.icon size={20} strokeWidth={1.7} aria-hidden="true" /><span>{page.label}</span>
        </Link>)}
      </div>
      {recent.length > 0 && <section className={styles.moreRecent}>
        <h2>最近查看</h2>
        <div onClick={closeOnLink(() => setMore(false))}><RecentList people={recent} /></div>
      </section>}
      <div className={styles.moreFooter}>
        <button type="button" onClick={cycleTheme}>{THEME_ICON[theme]}主题：{THEME_LABEL[theme]}</button>
        <button type="button" onClick={() => void logout()}><LogOut size={17} aria-hidden="true" />退出</button>
      </div>
    </Sheet>}

    {palette && <CommandPalette onClose={() => setPalette(false)} />}
  </div>;
}

function RecentList({ people }: { people: RecentPerson[] }) {
  return <ul className={styles.recentList} role="list">
    {people.map((person) => <li key={person.id}>
      <PersonLink href={`/profiles/${person.id}`} title={person.name}>
        <Avatar name={person.name} mediaId={person.avatarMediaId} size={24} round={person.type === "person"} />
        <span>{person.name}</span>
      </PersonLink>
    </li>)}
  </ul>;
}
