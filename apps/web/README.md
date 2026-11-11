# Monica Rust Web

The existing CRM React components, CSS modules, API calls, query cache, IndexedDB directory, search worker and browser paths are retained from CRM commit `6d59cfef4b1a12f678d809a00e37d8332be70b06`. Only the Next hosting/router shell was replaced by Vite and React Router. `src/router/` preserves the small navigation/link/lazy-component interface used by those components. The Noto Serif SC title font is bundled locally through Fontsource; no font service is contacted at runtime.

```sh
npm ci --prefix apps/web
npm run build --prefix apps/web
MONICA_WEB_ROOT="$PWD/apps/web/dist" target/debug/monica-server serve
```

Production needs no Node server. The Rust server serves `dist` and the existing `/api/v1/*` routes from the same origin. The backend must serve browser deep links as `index.html`; missing API routes must remain errors rather than receiving the SPA document. The development proxy defaults to `http://127.0.0.1:17494`, configurable with `MONICA_API_URL`. For dev authentication set the backend's `MONICA_ORIGIN` to the Vite origin.

The service worker caches only the static shell and same-origin assets; it never caches API or internal-control responses. Contact snapshots and post-search indexes continue to use IndexedDB. Router navigation preserves query strings, back/forward, local scroll preferences and profile deep links.

## Validation

```sh
npm run lint --prefix apps/web
npm run build --prefix apps/web
npm run test --prefix apps/web
python3 scripts/frontend-smoke.py
```

The normal Playwright suite uses synthetic HTTP fixtures only to verify routing, actual browser search workers, responsive layouts, authentication redirects and visible failure states. It does not establish backend feature parity. The separate real smoke creates a fresh PostgreSQL cluster, starts the actual Rust server and runs browser login → profile creation → profile detail → directory search → deep-link reload. It makes no request to an existing Monica database or NAS. Incomplete backend endpoints cause this test to fail visibly.

MapLibre was updated to 6.18 because the inherited version was flagged by npm audit; its ESM named exports are used. The imported map UI is otherwise unchanged. Both npm dependency trees reported zero audit findings at this implementation check.
