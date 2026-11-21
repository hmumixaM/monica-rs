# Interface design

Monica reads as a bound ledger. A deep-ink spine on the left holds the index; the
pages beside it are plain paper sheets with a hairline edge and no shadow; one
gamboge highlighter marks what needs attention now (the current page, the chosen
tab, today's dates). Source colours only describe where data came from.

Tokens live in `apps/web/app/styles/tokens.css` and are shared with the native
settings window (`apps/native-desktop/src/styles.css` imports the same file):
paper `#eef0f3`, sheet `#ffffff`, ink `#141a26`, secondary ink `#4d5666`, gamboge
`#f2b632`, danger `#c23b2a`, plus the `--spine-*` set for the sidebar and the
phone's top bar. Page titles, the greeting and the weekly letter use the bundled
Noto Serif SC; everything else uses the system Chinese sans. Dark theme keeps the
same roles with its own values.

## Navigation

The sidebar is grouped by what the user is doing, defined once in
`components/shell/nav.ts`:

| Group | Entries |
| --- | --- |
| (none) | 今天 |
| 人 | 联系人, 聊天, 动态 |
| 理解 | 问 Monica, 洞察 (概览 / 每周回顾 / 关系与足迹) |
| 整理 | 待整理 (资料建议 / 账号关联 / 照片里的人) |
| 数据 | 来源 (连接 / 同步与导入), 设置 |

An entry with several pages shows them as tabs above each page title. The shell
provides them through `HubContext` and `PageHeader` renders them, so every page
keeps its own URL and deep links are unchanged. The phone tab bar has 今天,
联系人, 聊天, 问 Monica and 更多; the More sheet and the command palette list every
page.

## Pages

- **今天** puts what happened on the left (last week's letter, friends' weekly
  notes) and what to do on the right (dates within two weeks, people to contact),
  with a one-line question box that opens 问 Monica with the question filled in.
- **问 Monica** is a chat workspace: conversations on the left, the transcript in
  the middle, people and the question at the bottom. Each answer lists what was
  looked up. Conversations can be created, deleted, exported and imported.
- **来源** is a rail of sources with the chosen one in detail: connected accounts
  with their state and actions, identity review, and for mail the add-mailbox
  form and the collected-mail list with include/ignore rules.
- **搜索** also searches chat messages across all conversations.

## Checks

`npm run build --prefix apps/web`, the Playwright suite (`npm run test --prefix
apps/web`), and the two real-server smokes (`scripts/frontend-smoke.py`,
`scripts/frontend-pages-smoke.py`). Review at 1440px and 390px in both themes.
