export interface ArgusEvent {
  seq: number;
  ts_wall: string;
  ts_monotonic: number;
  agent_id: string;
  type: string;
  pid?: number;
  path?: string;
  process_name?: string;
  [key: string]: unknown;
}

export interface EventsResponse {
  events: ArgusEvent[];
  count: number;
}
