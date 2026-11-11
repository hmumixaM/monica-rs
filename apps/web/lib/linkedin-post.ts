/** Keep native LinkedIn post identities; activity and ugcPost IDs are not aliases. */
export function linkedinPostUrl(content: unknown): string | undefined {
  if (!content || typeof content !== "object") return;
  const post = content as Record<string, unknown>;
  for (const value of [post.url, post.linkedin_url, post.permalink]) {
    if (typeof value !== "string") continue;
    try {
      const url = new URL(value);
      if (url.protocol === "https:" && ["www.linkedin.com", "linkedin.com"].includes(url.hostname)
        && !url.username && !url.password && !url.port
        && (/^\/posts\//.test(url.pathname) || /^\/feed\/update\/urn:li:(activity|ugcPost|share):\d+\/?$/.test(decodeURIComponent(url.pathname)))) return url.href;
    } catch { /* Historical records may have an invalid URL but a valid native ID. */ }
  }
  const urn = post.provider_post_id || post.source_key || post.urn;
  if (typeof urn === "string" && /^urn:li:(activity|ugcPost|share):\d+$/.test(urn)) {
    return `https://www.linkedin.com/feed/update/${urn}/`;
  }
}
