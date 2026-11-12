"use client";
import { useVirtualizer, useWindowVirtualizer } from '@tanstack/react-virtual';
import { type ReactNode, useLayoutEffect, useRef, useState } from 'react';

/** Stable item keys retain measurements as more history is appended. */
export function VirtualList({ items, anchor }: { items: { key: string; content: ReactNode }[]; anchor?: string }) {
  const root = useRef<HTMLDivElement>(null);
  const positioned = useRef<string | undefined>(undefined);
  const [margin, setMargin] = useState(0);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const [ready, setReady] = useState(false);
  useLayoutEffect(() => {
    let parent = root.current!.parentElement;
    while (parent && !['auto', 'scroll'].includes(getComputedStyle(parent).overflowY)) parent = parent.parentElement;
    setScroller(parent); setReady(true);
    const measure = () => setMargin(root.current!.getBoundingClientRect().top - (parent?.getBoundingClientRect().top ?? 0) + (parent?.scrollTop ?? window.scrollY));
    const observer = new ResizeObserver(measure);
    observer.observe(root.current!);
    if (parent) observer.observe(parent);
    measure();
    return () => observer.disconnect();
  }, []);
  const options = { count: items.length, estimateSize: () => 100, overscan: 12,
    scrollMargin: margin, getItemKey: (index: number) => items[index].key };
  const windowVirtualizer = useWindowVirtualizer({ ...options, enabled: ready && !scroller });
  const elementVirtualizer = useVirtualizer({ ...options, enabled: ready && !!scroller, getScrollElement: () => scroller });
  const virtualizer = scroller ? elementVirtualizer : windowVirtualizer;
  useLayoutEffect(() => {
    const index = anchor ? items.findIndex(item => item.key === anchor) : -1;
    if (ready && index >= 0 && positioned.current !== anchor) {
      virtualizer.scrollToIndex(index, { align: 'center' });
      positioned.current = anchor;
    }
  }, [anchor, items, virtualizer, ready]);
  return <div ref={root} style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
    {virtualizer.getVirtualItems().map(item => <div key={item.key} data-index={item.index} ref={virtualizer.measureElement}
      style={{ display: 'grid', gap: 3, position: 'absolute', top: item.start - margin, width: '100%' }}>{items[item.index].content}</div>)}
  </div>;
}
