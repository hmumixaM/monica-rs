"use client";

import { useEffect, useState } from "react";

import { apiFetch } from "../../lib/api";
import { Button } from "../ui/button";
import { Field, Input, Select } from "../ui/field";
import { Failure, Faint, Section } from "../ui/primitives";
import { message, request, useRefreshConnections } from "./api";
import styles from "./sources.module.css";

/** Pair a WhatsApp device by QR code. Nothing syncs before this explicit step. */
export function WhatsAppConnect() {
  const refresh = useRefreshConnections();
  const [connection, setConnection] = useState("");
  const [state, setState] = useState<{ state?: string; qr_data?: string; error?: string }>({});
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!connection) return;
    let active = true;
    const read = () => request(`/api/v1/source-connections/${connection}/whatsapp/status`).then((value) => { if (active) setState(value); })
      .catch((failure) => { if (active) setError(message(failure)); });
    void read();
    const timer = setInterval(read, 2500);
    return () => { active = false; clearInterval(timer); };
  }, [connection]);
  return <Section title="连接 WhatsApp 设备" panel>
    <Faint>连接后才会开始同步。用手机 WhatsApp 的「已连接的设备」扫描出现的二维码。</Faint>
    <form className={styles.form} onSubmit={async (event) => {
      event.preventDefault();
      const data = new FormData(event.currentTarget);
      setBusy(true); setError("");
      try {
        const value = await request<{ id?: string; connection_id?: string }>("/api/v1/whatsapp/connect", { external_account: data.get("account"), display_name: data.get("name") });
        setConnection(value.id || value.connection_id || ""); await refresh();
      } catch (failure) { setError(message(failure)); } finally { setBusy(false); }
    }}>
      <Field label="账号标识"><Input name="account" placeholder="个人 WhatsApp" required /></Field>
      <Field label="显示名称"><Input name="name" placeholder="我的 WhatsApp" required /></Field>
      <Button type="submit" variant="primary" disabled={busy}>{busy ? "正在连接…" : "连接设备"}</Button>
    </form>
    {state.state && <Faint>{state.state === "needs_bridge" ? "设备同步组件还没有就绪。仍然可以用下面的手机历史导入。" : state.state}</Faint>}
    {state.qr_data && <img className={styles.qr} src={state.qr_data} alt="WhatsApp 连接二维码" />}
    {(error || state.error) && <Failure>{error || state.error}</Failure>}
  </Section>;
}

/** Import a chat exported from the phone (TXT, or ZIP with attachments). */
export function WhatsAppExport() {
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  return <Section title="导入手机里的历史" panel>
    <Faint>选择手机导出的聊天 TXT，或包含附件的 ZIP。导出文件里的姓名只在这个会话里识别，导入后可以在账号下关联到人物。</Faint>
    <form className={styles.form} onSubmit={async (event) => {
      event.preventDefault();
      setBusy(true); setError(""); setStatus("正在读取导出文件…");
      try {
        const response = await apiFetch("/api/v1/whatsapp/phone-export", { method: "POST", body: new FormData(event.currentTarget) });
        const result = await response.json();
        if (!response.ok) throw new Error(result.detail || "导入失败");
        setStatus(`已读取 ${result.records_read} 条记录，后台正在整理到人物时间线。`);
      } catch (failure) { setStatus(""); setError(message(failure)); } finally { setBusy(false); }
    }}>
      <Field label="导出文件"><Input name="file" type="file" accept=".txt,.zip" required /></Field>
      <Field label="会话名称" hint="同一个会话请使用同一个名称"><Input name="conversation" required /></Field>
      <Field label="会话类型"><Select name="conversation_kind"><option value="group">群聊，或不确定</option><option value="direct">我和另一个人的私聊</option></Select></Field>
      <Field label="我在导出文件里的姓名"><Input name="owner" required /></Field>
      <Field label="日期顺序"><Select name="date_order"><option value="mdy">月 / 日 / 年</option><option value="dmy">日 / 月 / 年</option></Select></Field>
      <Field label="导出时所在的时区"><Input name="timezone" defaultValue={Intl.DateTimeFormat().resolvedOptions().timeZone} required /></Field>
      <Button type="submit" variant="primary" disabled={busy}>{busy ? "正在导入…" : "导入历史"}</Button>
    </form>
    {status && <Faint>{status}</Faint>}
    {error && <Failure>{error}</Failure>}
  </Section>;
}
