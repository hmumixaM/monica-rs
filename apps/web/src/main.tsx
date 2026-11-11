import React, { Suspense, lazy, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import "@fontsource/noto-serif-sc/600.css";
import "../app/styles/tokens.css";
import { PwaRegister } from "../components/pwa-register";
import { ToastProvider } from "../components/ui/toast";
import { QueryProvider } from "../lib/queries";
import { ScrollManager } from "./router/navigation";
const pages = import.meta.glob<{ default: React.ComponentType }>("../app/**/page.tsx");
const routes = Object.entries(pages).filter(([path]) => path !== "../app/page.tsx").map(([path, load]) => ({
  path: path.replace("../app", "").replace("/page.tsx", "").replace(/\[([^\]]+)\]/g, ":$1"),
  Component: lazy(load),
}));
function App() {
  useEffect(() => { document.title = "Monica Rust"; }, []);
  return <BrowserRouter><ScrollManager /><PwaRegister /><QueryProvider><ToastProvider>
    <Suspense fallback={<main aria-busy="true" style={{ padding: "2rem" }}>正在打开 Monica…</main>}>
      <Routes><Route path="/" element={<Navigate to="/dashboard" replace />} />
        {routes.map(({path, Component}) => <Route key={path} path={path} element={<Component />} />)}
        <Route path="*" element={<main style={{padding:"2rem"}}><h1>页面不存在</h1><a href="/dashboard">返回首页</a></main>} />
      </Routes>
    </Suspense>
  </ToastProvider></QueryProvider></BrowserRouter>;
}
createRoot(document.getElementById("root")!).render(<App />);
