"use client";

import {McpSettings,PasswordSettings} from "./mcp-settings";
import {Page, PageHeader} from "../ui/primitives";
import {AiStatus} from "./ai-status";
import {ModelSettings} from "./model-settings";
import {ArchivedProfiles} from "./archived-profiles";
import {Appearance} from "./appearance";
import {DeviceSessions} from "./device-sessions";
import {TaskStatus} from "./task-status";
import {TotpSettings} from "./totp-settings";
import {Tabs} from "../ui/tabs";
import {useRouter, useSearchParams} from "../../src/router/navigation";
import styles from "./settings.module.css";
const groups = [{id:'general',label:'常规'}, {id:'account',label:'账号与安全'}, {id:'ai',label:'AI 与 MCP'}, {id:'background',label:'后台状态'}];

export function SettingsView() {
  const params = useSearchParams();
  const router = useRouter();
  const tab = groups.some(g=>g.id===params.get('tab')) ? params.get('tab')! : 'general';
  return <Page width="narrow">
    <PageHeader title="设置" description="管理显示、账号安全、AI 服务和后台运行。" />
    <Tabs label="设置分类" items={groups} value={tab} onSelect={value=>{const next=new URLSearchParams(params);next.set('tab',value);router.replace('/settings?'+next.toString(),{scroll:false});}} />
    <div className={styles.group} hidden={tab!=='general'}><Appearance /><ArchivedProfiles /></div>
    <div className={styles.group} hidden={tab!=='account'}><PasswordSettings /><TotpSettings /><DeviceSessions /></div>
    <div className={styles.group} hidden={tab!=='ai'}><ModelSettings /><AiStatus /><McpSettings /></div>
    <div className={styles.group} hidden={tab!=='background'}><TaskStatus /></div>
  </Page>;
}
