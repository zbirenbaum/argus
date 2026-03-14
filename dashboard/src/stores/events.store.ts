import { batch, createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { createEventStream, fetchEvents } from "@/lib/api";
import type { ArgusEvent } from "@/types/events";

const [events, setEvents] = createStore<ArgusEvent[]>([]);
const [processes, setProcesses] = createStore<Record<number, string>>({});
const [selectedSeq, setSelectedSeq] = createSignal<number | null>(null);
const [connected, setConnected] = createSignal(false);

// Indexed lookup for selection — avoids O(n) .find()
const eventBySeq = new Map<number, ArgusEvent>();

let eventSource: EventSource | null = null;

// SSE batching — buffer events and flush on rAF
let pendingEvents: ArgusEvent[] = [];
let flushScheduled = false;

function getMaxSeq(): number | undefined {
  if (events.length === 0) return undefined;
  const last = events[events.length - 1];
  return last?.seq;
}

function scheduleFlush(): void {
  if (flushScheduled) return;
  flushScheduled = true;
  requestAnimationFrame(flushPendingEvents);
}

function flushPendingEvents(): void {
  flushScheduled = false;
  if (pendingEvents.length === 0) return;

  const toFlush = pendingEvents;
  pendingEvents = [];

  batch(() => {
    setEvents(
      produce((draft) => {
        for (const event of toFlush) {
          draft.push(event);
        }
      }),
    );

    for (const event of toFlush) {
      eventBySeq.set(event.seq, event);
      if (event.pid !== undefined && event.process_name !== undefined) {
        setProcesses(event.pid, event.process_name);
      }
    }
  });
}

export async function initialize(): Promise<void> {
  try {
    const initialEvents = await fetchEvents();

    for (const e of initialEvents) {
      eventBySeq.set(e.seq, e);
    }
    setEvents(initialEvents);

    const procMap: Record<number, string> = {};
    for (const e of initialEvents) {
      const name = e.process_name;
      if (e.pid !== undefined && name !== undefined) {
        procMap[e.pid] = name;
      }
    }
    setProcesses(procMap);

    connectStream();
  } catch (_e) {
    connectStream();
  }
}

function connectStream(): void {
  if (eventSource) {
    eventSource.close();
  }

  const afterSeq = getMaxSeq();
  eventSource = createEventStream(afterSeq);

  eventSource.onopen = () => {
    setConnected(true);
  };

  eventSource.onmessage = (msg) => {
    try {
      const event = JSON.parse(msg.data as string) as ArgusEvent;
      pendingEvents.push(event);
      scheduleFlush();
    } catch {
      // skip malformed messages
    }
  };

  eventSource.onerror = () => {
    setConnected(false);
  };
}

export function getEventBySeq(seq: number): ArgusEvent | undefined {
  return eventBySeq.get(seq);
}

export function selectEvent(seq: number): void {
  setSelectedSeq(seq);
}

export function clearSelection(): void {
  setSelectedSeq(null);
}

export function cleanupStream(): void {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
}

export { events, processes, selectedSeq, connected };
