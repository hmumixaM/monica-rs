import { forwardRef, type AnchorHTMLAttributes } from "react";
import { Link as RouterLink } from "react-router-dom";
type Props = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href"> & { href: string; replace?: boolean; scroll?: boolean; prefetch?: boolean };
const Link = forwardRef<HTMLAnchorElement, Props>(function Link({href, replace, scroll, prefetch: _prefetch, ...props}, ref) {
  if (/^(?:[a-z]+:|\/\/)/i.test(href) || props.download) return <a ref={ref} href={href} {...props} />;
  return <RouterLink ref={ref} to={href} replace={replace} state={{monicaScroll: scroll !== false}} {...props} />;
});
export default Link;
