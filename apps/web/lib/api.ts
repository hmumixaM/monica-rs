"use client";

export async function apiFetch(input: RequestInfo | URL, { propagateCsrfFailure, ...init }: RequestInit & { propagateCsrfFailure?: boolean } = {}) {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (!["GET", "HEAD", "OPTIONS"].includes(method)) {
    // Cookies are shared across ports. Ask this installation for its own token.
    const response = await fetch("/api/v1/auth/csrf", { credentials: "same-origin", signal: init.signal });
    if (propagateCsrfFailure && !response.ok) return response;
    if (response.ok) {
      const body = (await response.json()) as { csrf_token?: string };
      if (body.csrf_token) headers.set("X-CSRF-Token", body.csrf_token);
    }
  }
  return fetch(input, { ...init, headers, credentials: "same-origin" });
}
