"use client";

import Link from "@/router/link";
import { createContext, useContext } from "react";

import controls from "../ui/controls.module.css";
import type { NavEntry } from "./nav";

/** The pages that share the current sidebar entry, provided by the shell. */
export const HubContext = createContext<{ pages: NavEntry[]; active: string } | null>(null);

/** Tabs between the pages of one sidebar entry. Renders nothing on a page that stands alone. */
export function HubTabs() {
  const hub = useContext(HubContext);
  if (!hub) return null;
  return <nav className={controls.tabs} aria-label="本组页面" data-hub="">
    {hub.pages.map((page) => <Link key={page.id} href={page.href} aria-current={page.id === hub.active ? "page" : undefined}>
      <page.icon size={15} strokeWidth={1.8} aria-hidden="true" />{page.label}
    </Link>)}
  </nav>;
}
