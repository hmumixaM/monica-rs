"use client";

import { useWindowVirtualizer } from "@tanstack/react-virtual";
import { Children, isValidElement, type ReactNode, useLayoutEffect, useMemo, useRef, useState } from "react";
import styles from "./feed.module.css";

/** Keep only visible cards mounted. Existing row measurements survive page appends. */
export function Masonry({ children, columns }: { children: ReactNode; columns?: "compact" }) {
  const root = useRef<HTMLDivElement>(null);
  const items = useMemo(() => Children.toArray(children), [children]);
  const [layout, setLayout] = useState({ width: 0, lanes: 2, gap: 12, margin: 0 });
  useLayoutEffect(() => {
    const container = root.current!;
    const measure = () => {
      const style = getComputedStyle(container);
      const next = { width: container.clientWidth, lanes: Math.max(1, Number(style.getPropertyValue("--feed-columns")) || 2),
        gap: parseFloat(style.getPropertyValue("--feed-gap")) || 12, margin: container.getBoundingClientRect().top + window.scrollY };
      setLayout(old => Object.keys(next).every(key => old[key as keyof typeof old] === next[key as keyof typeof next]) ? old : next);
    };
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    measure();
    return () => observer.disconnect();
  }, []);
  const virtualizer = useWindowVirtualizer({ count: items.length, lanes: layout.lanes, gap: layout.gap,
    estimateSize: () => 420, overscan: 6, scrollMargin: layout.margin,
    getItemKey: index => isValidElement(items[index]) ? items[index].key ?? index : index });
  const width = (layout.width - layout.gap * (layout.lanes - 1)) / layout.lanes;
  return <div className={styles.masonryHost}><div className={styles.masonry} data-columns={columns} data-positioned="" ref={root}
    style={{ height: virtualizer.getTotalSize(), display: "block" }}>
    {virtualizer.getVirtualItems().map(item => <div key={item.key} data-index={item.index} ref={virtualizer.measureElement}
      style={{ position: "absolute", width, left: item.lane * (width + layout.gap), top: item.start - layout.margin }}>
      {items[item.index]}
    </div>)}
  </div></div>;
}
