"use client";

import {Page, PageHeader} from "../ui/primitives";
import {AiStatus} from "./ai-status";
import {ModelSettings} from "./model-settings";
import {ArchivedProfiles} from "./archived-profiles";
import {Appearance} from "./appearance";
import {DeviceSessions} from "./device-sessions";
import {TaskStatus} from "./task-status";
import {TotpSettings} from "./totp-settings";

export function SettingsView() {
  return <Page width="narrow">
    <PageHeader title="设置" />
    <Appearance />
    <ArchivedProfiles />
    <DeviceSessions />
    <TotpSettings />
    <ModelSettings />
    <AiStatus />
    <TaskStatus />
  </Page>;
}
