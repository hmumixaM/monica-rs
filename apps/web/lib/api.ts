"use client";

async function csrfToken(signal?: AbortSignal | null) {
  // The server selects the installation's cookie namespace. Cookies are shared
  // across ports, so reading a fixed name can pick up another Monica instance.
  const response = await fetch("/api/v1/auth/csrf", { credentials: "same-origin", signal });
  if (!response.ok) return "";
  const body = (await response.json()) as { csrf_token?: string };
  return body.csrf_token ?? "";
}

export async function apiFetch(input: RequestInfo | URL, init: RequestInit = {}) {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (!["GET", "HEAD", "OPTIONS"].includes(method)) {
    const token = await csrfToken(init.signal);
    if (token) headers.set("X-CSRF-Token", token);
  }
  return fetch(input, { ...init, headers, credentials: "same-origin" });
}
