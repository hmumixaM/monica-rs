import { AskView } from "../../components/ask/ask-view";
import { AppShell } from "../../components/app-shell";
import { Suspense } from "react";
export default function Page() { return <AppShell active="ask"><Suspense fallback={<p>正在准备会话…</p>}><AskView /></Suspense></AppShell>; }
