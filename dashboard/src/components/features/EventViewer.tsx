import {
  clearSelection,
  events,
  getEventBySeq,
  processes,
  selectEvent,
  selectedSeq,
} from "@stores/events.store";
import { formatTimeMs as formatTime } from "@lib/format";
import { cn } from "@utils/cn";
import { createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { For, Show } from "solid-js/web";
import type { ArgusEvent } from "@/types/events";

// --- Row types for the flat virtual list ---

interface PidHeaderRow {
  kind: "pid-header";
  pid: number;
  name: string;
  count: number;
  key: string;
}

interface TypeHeaderRow {
  kind: "type-header";
  pid: number;
  type: string;
  count: number;
  key: string;
}

interface EventRow {
  kind: "event";
  event: ArgusEvent;
  key: string;
}

type FlatRow = PidHeaderRow | TypeHeaderRow | EventRow;

// --- Constants ---

const ROW_HEIGHT = 28;
const OVERSCAN = 20;

// --- Helpers ---

const FS_TYPES = new Set([
  "read",
  "write",
  "rename",
  "unlink",
  "mkdir",
  "rmdir",
  "chmod",
  "truncate",
  "link",
  "symlink",
]);

function eventContext(event: ArgusEvent): string | undefined {
  if (event.type === "exec") {
    const binary = event["binary"];
    return typeof binary === "string" ? binary.split("/").pop() : undefined;
  }
  if (FS_TYPES.has(event.type) && event.path) {
    const p = event.path;
    const name = p.split("/").pop() ?? p;
    const isDir = event.type === "mkdir" || event.type === "rmdir";
    return isDir ? `${name}/` : name;
  }
  return undefined;
}

// --- Main component ---

export function EventViewer() {
  const selectedEvent = createMemo<ArgusEvent | undefined>(() => {
    const seq = selectedSeq();
    if (seq === null) return undefined;
    return getEventBySeq(seq);
  });

  return (
    <div class="flex h-full w-full flex-col">
      <div class="flex min-h-0 flex-1">
        <VirtualSidebar />
        <DetailPanel event={selectedEvent()} />
      </div>
    </div>
  );
}

// --- Virtual sidebar ---

function VirtualSidebar() {
  let containerRef!: HTMLDivElement;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportHeight, setViewportHeight] = createSignal(0);

  // Track which sections are open: "pid:3" or "type:3:read"
  const [openSections, setOpenSections] = createSignal<Set<string>>(new Set());

  function toggleSection(key: string) {
    setOpenSections((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  // Group events by pid → type
  const grouped = createMemo(() => {
    const pidMap = new Map<number, Map<string, ArgusEvent[]>>();

    for (const event of events) {
      const pid = event.pid ?? 0;
      let typeMap = pidMap.get(pid);
      if (!typeMap) {
        typeMap = new Map<string, ArgusEvent[]>();
        pidMap.set(pid, typeMap);
      }
      let list = typeMap.get(event.type);
      if (!list) {
        list = [];
        typeMap.set(event.type, list);
      }
      list.push(event);
    }

    const pids = [...pidMap.keys()].sort((a, b) => a - b);
    return { pidMap, pids };
  });

  // Flatten into virtual rows based on open/closed state
  const flatRows = createMemo<FlatRow[]>(() => {
    const { pidMap, pids } = grouped();
    const open = openSections();
    const rows: FlatRow[] = [];

    for (const pid of pids) {
      const typeMap = pidMap.get(pid)!;
      let pidCount = 0;
      for (const evts of typeMap.values()) {
        pidCount += evts.length;
      }
      const name = processes[pid] ?? `PID ${pid}`;
      const pidKey = `pid:${pid}`;

      rows.push({ kind: "pid-header", pid, name, count: pidCount, key: pidKey });

      if (open.has(pidKey)) {
        for (const [type, evts] of typeMap) {
          const typeKey = `type:${pid}:${type}`;
          rows.push({ kind: "type-header", pid, type, count: evts.length, key: typeKey });

          if (open.has(typeKey)) {
            for (const event of evts) {
              rows.push({ kind: "event", event, key: `evt:${event.seq}` });
            }
          }
        }
      }
    }

    return rows;
  });

  // Compute visible range
  const visibleRange = createMemo(() => {
    const top = scrollTop();
    const height = viewportHeight();
    const total = flatRows().length;

    const startIdx = Math.max(0, Math.floor(top / ROW_HEIGHT) - OVERSCAN);
    const endIdx = Math.min(total, Math.ceil((top + height) / ROW_HEIGHT) + OVERSCAN);

    return { startIdx, endIdx };
  });

  const totalHeight = createMemo(() => flatRows().length * ROW_HEIGHT);

  const visibleRows = createMemo(() => {
    const { startIdx, endIdx } = visibleRange();
    return flatRows().slice(startIdx, endIdx);
  });

  const offsetY = createMemo(() => visibleRange().startIdx * ROW_HEIGHT);

  onMount(() => {
    setViewportHeight(containerRef.offsetHeight);

    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setViewportHeight(entry.contentRect.height);
      }
    });
    observer.observe(containerRef);
    onCleanup(() => observer.disconnect());
  });

  function handleScroll() {
    setScrollTop(containerRef.scrollTop);
  }

  return (
    <aside
      ref={containerRef}
      onScroll={handleScroll}
      class={cn(
        "w-[350px] shrink-0 overflow-y-auto border-r",
        "border-[hsl(var(--border))] bg-[hsl(var(--background))]",
      )}
    >
      <div class="p-2">
        <h2 class="px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-[hsl(var(--muted-foreground))]">
          Events
        </h2>
      </div>
      <Show when={flatRows().length > 0} fallback={<EmptyState />}>
        <div style={{ height: `${totalHeight()}px`, position: "relative" }}>
          <div
            style={{
              transform: `translateY(${offsetY()}px)`,
              position: "absolute",
              left: "0",
              right: "0",
            }}
          >
            <For each={visibleRows()}>
              {(row) => {
                if (row.kind === "pid-header") {
                  return (
                    <PidHeaderRowView
                      row={row}
                      isOpen={openSections().has(row.key)}
                      onToggle={() => toggleSection(row.key)}
                    />
                  );
                }
                if (row.kind === "type-header") {
                  return (
                    <TypeHeaderRowView
                      row={row}
                      isOpen={openSections().has(row.key)}
                      onToggle={() => toggleSection(row.key)}
                    />
                  );
                }
                return <EventRowView event={row.event} />;
              }}
            </For>
          </div>
        </div>
      </Show>
    </aside>
  );
}

function EmptyState() {
  return <p class="px-2 py-4 text-sm text-[hsl(var(--muted-foreground))]">No events yet</p>;
}

// --- Row renderers ---

function PidHeaderRowView(props: { row: PidHeaderRow; isOpen: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      style={{ height: `${ROW_HEIGHT}px` }}
      class={cn(
        "flex w-full items-center gap-1.5 px-2 text-sm font-medium",
        "hover:bg-[hsl(var(--muted))] transition-colors rounded-[var(--radius-sm)]",
      )}
      onClick={props.onToggle}
    >
      <svg
        aria-hidden="true"
        class={cn("h-3.5 w-3.5 shrink-0 transition-transform", props.isOpen && "rotate-90")}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <path d="M9 18l6-6-6-6" />
      </svg>
      <span class="font-mono text-xs">{props.row.pid}</span>
      <span class="truncate">{props.row.name}</span>
      <span
        class={cn(
          "ml-auto shrink-0 rounded-full px-1.5 py-0.5 text-xs",
          "bg-[hsl(var(--secondary))] text-[hsl(var(--secondary-foreground))]",
        )}
      >
        {props.row.count}
      </span>
    </button>
  );
}

function TypeHeaderRowView(props: { row: TypeHeaderRow; isOpen: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      style={{ height: `${ROW_HEIGHT}px` }}
      class={cn(
        "flex w-full items-center gap-1.5 pl-5 pr-2 text-sm font-medium",
        "hover:bg-[hsl(var(--muted))] transition-colors rounded-[var(--radius-sm)]",
      )}
      onClick={props.onToggle}
    >
      <svg
        aria-hidden="true"
        class={cn("h-3.5 w-3.5 shrink-0 transition-transform", props.isOpen && "rotate-90")}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <path d="M9 18l6-6-6-6" />
      </svg>
      <span class="text-xs">{props.row.type}</span>
      <span class="ml-auto text-xs text-[hsl(var(--muted-foreground))]">{props.row.count}</span>
    </button>
  );
}

function EventRowView(props: { event: ArgusEvent }) {
  const isSelected = () => selectedSeq() === props.event.seq;
  const context = () => eventContext(props.event);

  function handleClick() {
    if (isSelected()) {
      clearSelection();
    } else {
      selectEvent(props.event.seq);
    }
  }

  return (
    <button
      type="button"
      style={{ height: `${ROW_HEIGHT}px` }}
      class={cn(
        "flex w-full items-center gap-2 rounded-[var(--radius-sm)] pl-8 pr-2 text-left text-xs",
        "hover:bg-[hsl(var(--muted))] transition-colors cursor-pointer",
        isSelected() && "bg-[hsl(var(--accent))]",
      )}
      onClick={handleClick}
    >
      <span class="shrink-0 font-mono text-[hsl(var(--muted-foreground))]">#{props.event.seq}</span>
      <span class="shrink-0 font-mono">{formatTime(props.event.ts_wall)}</span>
      <Show when={context()}>
        {(ctx) => (
          <span class="truncate font-mono text-[hsl(var(--muted-foreground))]">{ctx()}</span>
        )}
      </Show>
    </button>
  );
}

// --- Detail panel ---

function DetailPanel(props: { event: ArgusEvent | undefined }) {
  return (
    <main class="flex-1 overflow-y-auto bg-[hsl(var(--background))] p-6">
      <Show
        when={props.event}
        fallback={
          <div class="flex h-full items-center justify-center">
            <p class="text-[hsl(var(--muted-foreground))]">Select an event</p>
          </div>
        }
      >
        {(event) => (
          <div>
            <div class="mb-4 flex items-baseline gap-3">
              <h2 class="text-lg font-semibold">{event().type}</h2>
              <Show when={event().process_name}>
                {(name) => (
                  <span class="text-sm text-[hsl(var(--muted-foreground))]">{name()}</span>
                )}
              </Show>
              <span class="font-mono text-xs text-[hsl(var(--muted-foreground))]">
                seq {event().seq}
              </span>
            </div>
            <Show when={event().path}>
              <p class="mb-3 font-mono text-sm text-[hsl(var(--muted-foreground))]">
                {event().path}
              </p>
            </Show>
            <pre class="overflow-x-auto rounded-[var(--radius-lg)] bg-[hsl(var(--secondary))] p-4 font-mono text-sm text-[hsl(var(--secondary-foreground))]">
              {JSON.stringify(event(), null, 2)}
            </pre>
          </div>
        )}
      </Show>
    </main>
  );
}
