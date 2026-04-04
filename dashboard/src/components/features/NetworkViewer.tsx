import { formatTimeMs as formatTime } from "@lib/format";
import {
  clearSelection,
  events,
  getEventBySeq,
  selectEvent,
  selectedSeq,
} from "@stores/events.store";
import { cn } from "@utils/cn";
import { createMemo, createSignal, type JSX, onCleanup, onMount } from "solid-js";
import { For, Match, Show, Switch } from "solid-js/web";
import type { ArgusEvent } from "@/types/events";

// --- Constants ---

const ROW_HEIGHT = 32;
const OVERSCAN = 20;

const NET_TYPES = new Set([
  "socket",
  "connect",
  "accept",
  "tls_keys",
  "http_request",
  "http_response",
]);

// --- Helpers ---

function statusColor(status: number): string {
  if (status >= 200 && status < 300) return "text-green-500";
  if (status >= 300 && status < 400) return "text-yellow-500";
  if (status >= 400 && status < 500) return "text-orange-500";
  if (status >= 500) return "text-red-500";
  return "text-[hsl(var(--muted-foreground))]";
}

function methodColor(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "text-blue-400";
    case "POST":
      return "text-green-400";
    case "PUT":
      return "text-yellow-400";
    case "PATCH":
      return "text-yellow-400";
    case "DELETE":
      return "text-red-400";
    default:
      return "text-[hsl(var(--foreground))]";
  }
}

/** Extract a short host from a URL. */
function urlHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** Extract path from a URL. */
function urlPath(url: string): string {
  try {
    const u = new URL(url);
    return u.pathname + u.search;
  } catch {
    return url;
  }
}

/** Group an http_request with its immediately following http_response. */
interface HttpTransaction {
  request: ArgusEvent;
  response: ArgusEvent | undefined;
}

// --- Main component ---

export function NetworkViewer() {
  const selectedEvent = createMemo<ArgusEvent | undefined>(() => {
    const seq = selectedSeq();
    if (seq === null) return undefined;
    return getEventBySeq(seq);
  });

  return (
    <div class="flex h-full w-full flex-col">
      <div class="flex min-h-0 flex-1">
        <NetworkSidebar />
        <NetworkDetailPanel event={selectedEvent()} />
      </div>
    </div>
  );
}

// --- Sidebar: grouped HTTP transactions ---

function NetworkSidebar() {
  let containerRef!: HTMLDivElement;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportHeight, setViewportHeight] = createSignal(0);
  const [filter, setFilter] = createSignal("");

  // Build HTTP transactions: pair requests with responses via flow_id
  // biome-ignore lint/complexity/noExcessiveCognitiveComplexity: pairing logic requires many branches
  const transactions = createMemo<HttpTransaction[]>(() => {
    const netEvents = events.filter((e) => NET_TYPES.has(e.type));

    // Index responses by flow_id for O(1) lookup
    const responseByFlowId = new Map<string, ArgusEvent>();
    for (const ev of netEvents) {
      if (ev.type === "http_response" && typeof ev.flow_id === "string") {
        responseByFlowId.set(ev.flow_id, ev);
      }
    }

    const pairedResponseSeqs = new Set<number>();
    const txns: HttpTransaction[] = [];

    // First pass: pair requests with responses
    for (const ev of netEvents) {
      if (ev.type === "http_request") {
        const flowId = ev.flow_id;
        const matched = flowId ? responseByFlowId.get(flowId) : undefined;
        if (matched) {
          pairedResponseSeqs.add(matched.seq);
        }
        txns.push({ request: ev, response: matched });
      }
    }

    // Second pass: orphaned responses and non-HTTP events
    for (const ev of netEvents) {
      if (ev.type === "http_response" && !pairedResponseSeqs.has(ev.seq)) {
        txns.push({ request: ev, response: undefined });
      } else if (ev.type !== "http_request" && ev.type !== "http_response") {
        txns.push({ request: ev, response: undefined });
      }
    }

    return txns;
  });

  const filtered = createMemo(() => {
    const q = filter().toLowerCase();
    if (!q) return transactions();
    return transactions().filter((txn) => {
      const url = String(txn.request.url ?? "").toLowerCase();
      const method = String(txn.request.method ?? txn.request.type).toLowerCase();
      const host = url ? urlHost(url).toLowerCase() : "";
      return (
        url.includes(q) || method.includes(q) || host.includes(q) || txn.request.type.includes(q)
      );
    });
  });

  // Virtual scroll
  const totalHeight = createMemo(() => filtered().length * ROW_HEIGHT);

  const visibleRange = createMemo(() => {
    const top = scrollTop();
    const height = viewportHeight();
    const total = filtered().length;
    const startIdx = Math.max(0, Math.floor(top / ROW_HEIGHT) - OVERSCAN);
    const endIdx = Math.min(total, Math.ceil((top + height) / ROW_HEIGHT) + OVERSCAN);
    return { startIdx, endIdx };
  });

  const visibleRows = createMemo(() => {
    const { startIdx, endIdx } = visibleRange();
    return filtered().slice(startIdx, endIdx);
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

  return (
    <aside class="flex w-[480px] shrink-0 flex-col border-r border-[hsl(var(--border))] bg-[hsl(var(--background))]">
      {/* Filter bar */}
      <div class="flex items-center gap-2 border-b border-[hsl(var(--border))] px-3 py-2">
        <svg
          aria-label="Search"
          class="h-4 w-4 shrink-0 text-[hsl(var(--muted-foreground))]"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <circle cx="11" cy="11" r="8" />
          <path d="M21 21l-4.35-4.35" />
        </svg>
        <input
          type="text"
          placeholder="Filter by URL, method, host..."
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
          class={cn(
            "flex-1 bg-transparent text-sm outline-none",
            "placeholder:text-[hsl(var(--muted-foreground))]",
          )}
        />
        <span class="shrink-0 text-xs font-mono text-[hsl(var(--muted-foreground))]">
          {filtered().length}
        </span>
      </div>

      {/* Column header */}
      <div class="flex items-center gap-2 border-b border-[hsl(var(--border))] px-3 py-1 text-xs font-medium uppercase tracking-wider text-[hsl(var(--muted-foreground))]">
        <span class="w-14 shrink-0">Method</span>
        <span class="w-10 shrink-0">Status</span>
        <span class="flex-1">URL</span>
        <span class="w-16 shrink-0 text-right">Time</span>
      </div>

      {/* Virtual list */}
      <div
        ref={containerRef}
        onScroll={() => setScrollTop(containerRef.scrollTop)}
        class="flex-1 overflow-y-auto"
      >
        <Show
          when={filtered().length > 0}
          fallback={
            <p class="px-3 py-4 text-sm text-[hsl(var(--muted-foreground))]">No network events</p>
          }
        >
          <div style={{ height: `${totalHeight()}px`, position: "relative" }}>
            <div
              style={{
                transform: `translateY(${offsetY()}px)`,
                position: "absolute",
                left: "0",
                right: "0",
              }}
            >
              <For each={visibleRows()}>{(txn) => <TransactionRow txn={txn} />}</For>
            </div>
          </div>
        </Show>
      </div>
    </aside>
  );
}

// --- Transaction row ---

function TransactionRow(props: { txn: HttpTransaction }) {
  const isHttpReq = () => props.txn.request.type === "http_request";
  const method = () => String(props.txn.request.method ?? props.txn.request.type).toUpperCase();
  const url = () => String(props.txn.request.url ?? "");
  const status = () => (props.txn.response ? Number(props.txn.response.status ?? 0) : 0);
  const isSelected = () => {
    const seq = selectedSeq();
    return (
      seq === props.txn.request.seq ||
      (props.txn.response !== undefined && seq === props.txn.response.seq)
    );
  };

  function handleClick() {
    if (isSelected()) {
      clearSelection();
    } else {
      selectEvent(props.txn.request.seq);
    }
  }

  return (
    <button
      type="button"
      style={{ height: `${ROW_HEIGHT}px` }}
      class={cn(
        "flex w-full items-center gap-2 px-3 text-left text-xs transition-colors",
        "hover:bg-[hsl(var(--muted))] cursor-pointer",
        isSelected() && "bg-[hsl(var(--accent))]",
      )}
      onClick={handleClick}
    >
      <Show
        when={isHttpReq()}
        fallback={
          <span class="w-14 shrink-0 font-mono text-[hsl(var(--muted-foreground))]">
            {props.txn.request.type}
          </span>
        }
      >
        <span class={cn("w-14 shrink-0 font-mono font-semibold", methodColor(method()))}>
          {method()}
        </span>
      </Show>

      <Show when={isHttpReq()} fallback={<span class="w-10 shrink-0" />}>
        <Show
          when={status() > 0}
          fallback={
            <span class="w-10 shrink-0 text-[hsl(var(--muted-foreground))] italic">...</span>
          }
        >
          <span class={cn("w-10 shrink-0 font-mono font-semibold", statusColor(status()))}>
            {status()}
          </span>
        </Show>
      </Show>

      <Show
        when={isHttpReq()}
        fallback={
          <span class="flex-1 truncate font-mono text-[hsl(var(--muted-foreground))]">
            {props.txn.request.type === "connect"
              ? `${props.txn.request.remote_addr}:${props.txn.request.remote_port}`
              : props.txn.request.type === "socket"
                ? `${props.txn.request.domain} ${props.txn.request.sock_type}`
                : ""}
          </span>
        }
      >
        <span class="flex-1 truncate font-mono" title={url()}>
          <span class="text-[hsl(var(--muted-foreground))]">{urlHost(url())}</span>
          <span>{urlPath(url())}</span>
        </span>
      </Show>

      <span class="w-16 shrink-0 text-right font-mono text-[hsl(var(--muted-foreground))]">
        {formatTime(props.txn.request.ts_wall)}
      </span>
    </button>
  );
}

// --- Detail panel ---

function NetworkDetailPanel(props: { event: ArgusEvent | undefined }) {
  return (
    <main class="flex-1 overflow-y-auto bg-[hsl(var(--background))] p-6">
      <Show
        when={props.event}
        fallback={
          <div class="flex h-full items-center justify-center">
            <p class="text-[hsl(var(--muted-foreground))]">Select a request</p>
          </div>
        }
      >
        {(event) => (
          <Switch fallback={<GenericDetail event={event()} />}>
            <Match when={event().type === "http_request"}>
              <HttpRequestDetail event={event()} />
            </Match>
            <Match when={event().type === "http_response"}>
              <HttpResponseDetail event={event()} />
            </Match>
          </Switch>
        )}
      </Show>
    </main>
  );
}

/** Parse inline headers JSON string into [name, value] pairs. */
function parseHeaders(raw: unknown): [string, string][] {
  if (typeof raw !== "string") return [];
  try {
    return JSON.parse(raw) as [string, string][];
  } catch {
    return [];
  }
}

/** Try to pretty-print a body string if it's JSON, otherwise return as-is. */
function formatBody(raw: unknown): string {
  if (typeof raw !== "string") return "";
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

function HttpRequestDetail(props: { event: ArgusEvent }) {
  const method = () => String(props.event.method ?? "");
  const url = () => String(props.event.url ?? "");
  const reqHeaders = () => parseHeaders(props.event.headers);
  const reqBody = () => props.event.body;

  // Find matching response via flow_id, fall back to positional
  // biome-ignore lint/complexity/noExcessiveCognitiveComplexity: matching logic with fallback
  const matchingResponse = createMemo(() => {
    const flowId = props.event.flow_id;
    if (flowId) {
      for (const ev of events) {
        if (ev.type === "http_response" && ev.flow_id === flowId) {
          return ev;
        }
      }
    }
    // Legacy fallback: next event with same pid
    const seq = props.event.seq;
    for (let i = 0; i < events.length; i++) {
      if (events[i]?.seq === seq && i + 1 < events.length) {
        const next = events[i + 1];
        if (!next) break;
        if (next.type === "http_response" && next.pid === props.event.pid) {
          return next;
        }
        break;
      }
    }
    return undefined;
  });

  return (
    <div class="space-y-4">
      <div class="flex items-baseline gap-3">
        <span class={cn("text-lg font-bold font-mono", methodColor(method()))}>{method()}</span>
        <span class="font-mono text-sm break-all">{url()}</span>
      </div>

      <div class="flex gap-4 text-xs text-[hsl(var(--muted-foreground))]">
        <span>PID {props.event.pid}</span>
        <Show when={props.event.process_name}>{(name) => <span>{name()}</span>}</Show>
        <span>seq {props.event.seq}</span>
        <span>{formatTime(props.event.ts_wall)}</span>
      </div>

      <Show when={reqHeaders().length > 0}>
        <Section title="Request Headers">
          <HeadersTable headers={reqHeaders()} />
        </Section>
      </Show>

      <Show when={reqBody()}>
        {(body) => (
          <Section title="Request Body">
            <pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs">
              {formatBody(body())}
            </pre>
          </Section>
        )}
      </Show>

      <Show when={matchingResponse()}>{(resp) => <ResponseSection resp={resp()} />}</Show>
    </div>
  );
}

function ResponseSection(props: { resp: ArgusEvent }) {
  const status = () => Number(props.resp.status ?? 0);
  const respHeaders = () => parseHeaders(props.resp.headers);
  const respBody = () => props.resp.body;

  return (
    <div class="mt-6 border-t border-[hsl(var(--border))] pt-4 space-y-4">
      <div class="flex items-baseline gap-3">
        <span class="text-sm font-semibold uppercase text-[hsl(var(--muted-foreground))]">
          Response
        </span>
        <span class={cn("text-lg font-bold font-mono", statusColor(status()))}>{status()}</span>
      </div>

      <Show when={respHeaders().length > 0}>
        <Section title="Response Headers">
          <HeadersTable headers={respHeaders()} />
        </Section>
      </Show>

      <Show when={respBody()}>
        {(body) => (
          <Section title="Response Body">
            <pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs max-h-[600px] overflow-y-auto">
              {formatBody(body())}
            </pre>
          </Section>
        )}
      </Show>
    </div>
  );
}

function HttpResponseDetail(props: { event: ArgusEvent }) {
  const status = () => Number(props.event.status ?? 0);
  const headers = () => parseHeaders(props.event.headers);
  const body = () => props.event.body;

  return (
    <div class="space-y-4">
      <div class="flex items-baseline gap-3">
        <span class="text-sm font-semibold uppercase text-[hsl(var(--muted-foreground))]">
          Response
        </span>
        <span class={cn("text-lg font-bold font-mono", statusColor(status()))}>{status()}</span>
      </div>

      <div class="flex gap-4 text-xs text-[hsl(var(--muted-foreground))]">
        <span>PID {props.event.pid}</span>
        <span>seq {props.event.seq}</span>
        <span>{formatTime(props.event.ts_wall)}</span>
      </div>

      <Show when={headers().length > 0}>
        <Section title="Headers">
          <HeadersTable headers={headers()} />
        </Section>
      </Show>

      <Show when={body()}>
        {(b) => (
          <Section title="Body">
            <pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs max-h-[600px] overflow-y-auto">
              {formatBody(b())}
            </pre>
          </Section>
        )}
      </Show>
    </div>
  );
}

function HeadersTable(props: { headers: [string, string][] }) {
  return (
    <div class="space-y-0.5">
      <For each={props.headers}>
        {(h) => (
          <div class="flex gap-2 font-mono text-xs">
            <span class="shrink-0 font-semibold">{h[0]}:</span>
            <span class="break-all text-[hsl(var(--muted-foreground))]">{h[1]}</span>
          </div>
        )}
      </For>
    </div>
  );
}

function GenericDetail(props: { event: ArgusEvent }) {
  return (
    <div class="space-y-4">
      <div class="flex items-baseline gap-3">
        <h2 class="text-lg font-semibold">{props.event.type}</h2>
        <span class="font-mono text-xs text-[hsl(var(--muted-foreground))]">
          seq {props.event.seq}
        </span>
      </div>

      <div class="flex gap-4 text-xs text-[hsl(var(--muted-foreground))]">
        <span>PID {props.event.pid}</span>
        <Show when={props.event.process_name}>{(name) => <span>{name()}</span>}</Show>
        <span>{formatTime(props.event.ts_wall)}</span>
      </div>

      <Section title="Raw Event">
        <pre class="overflow-x-auto font-mono text-xs">{JSON.stringify(props.event, null, 2)}</pre>
      </Section>
    </div>
  );
}

function Section(props: { title: string; children: JSX.Element }) {
  return (
    <div>
      <h3 class="mb-1 text-xs font-semibold uppercase tracking-wider text-[hsl(var(--muted-foreground))]">
        {props.title}
      </h3>
      <div class="rounded-[var(--radius-lg)] bg-[hsl(var(--secondary))] p-3 text-[hsl(var(--secondary-foreground))]">
        {props.children}
      </div>
    </div>
  );
}
