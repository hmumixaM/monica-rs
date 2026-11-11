import { useCallback, useEffect, useMemo } from "react";
import { useLocation, useNavigate, useNavigationType, useSearchParams as routerSearchParams } from "react-router-dom";
type Options = { scroll?: boolean };
export function useRouter() {
  const navigate = useNavigate();
  const push = useCallback((url: string, options?: Options) => navigate(url, { state: { monicaScroll: options?.scroll !== false } }), [navigate]);
  const replace = useCallback((url: string, options?: Options) => navigate(url, { replace: true, state: { monicaScroll: options?.scroll !== false } }), [navigate]);
  return useMemo(() => ({ push, replace, back: () => navigate(-1), forward: () => navigate(1), refresh: () => window.location.reload(), prefetch: (_url: string) => undefined }), [push, replace, navigate]);
}
export function usePathname() { return useLocation().pathname; }
export function useSearchParams() { return routerSearchParams()[0]; }
const positions = new Map<string, [number, number]>();
export function ScrollManager() {
  const location = useLocation();
  const kind = useNavigationType();
  useEffect(() => {
    if (kind === "POP" && positions.has(location.key)) window.scrollTo(...positions.get(location.key)!);
    else if (location.hash) requestAnimationFrame(() => document.getElementById(decodeURIComponent(location.hash.slice(1)))?.scrollIntoView());
    else if (location.state?.monicaScroll !== false) window.scrollTo(0, 0);
    return () => { positions.set(location.key, [window.scrollX, window.scrollY]); if (positions.size > 100) positions.delete(positions.keys().next().value!); };
  }, [location.key, location.hash, location.state, kind]);
  return null;
}
