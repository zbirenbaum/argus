import type { ArgusEvent, EventsResponse } from "@/types/events";

const API_BASE = "http://localhost:8000";

export async function fetchEvents(afterSeq?: number): Promise<ArgusEvent[]> {
  const params = new URLSearchParams({ limit: "10000" });
  if (afterSeq !== undefined) {
    params.set("after_seq", String(afterSeq));
  }
  const res = await fetch(`${API_BASE}/events?${params.toString()}`);
  if (!res.ok) {
    throw new Error(`Failed to fetch events: ${res.status}`);
  }
  const data = (await res.json()) as EventsResponse;
  return data.events;
}

export function createEventStream(afterSeq?: number): EventSource {
  const params = new URLSearchParams();
  if (afterSeq !== undefined) {
    params.set("after_seq", String(afterSeq));
  }
  const query = params.toString();
  const url = query ? `${API_BASE}/events/stream?${query}` : `${API_BASE}/events/stream`;
  return new EventSource(url);
}
