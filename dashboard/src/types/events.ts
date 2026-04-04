export interface ArgusEvent {
  seq: number;
  ts_wall: string;
  ts_monotonic: number;
  agent_id: string;
  type: string;
  pid?: number;
  path?: string;
  process_name?: string;
  // exec
  binary?: string;
  // agent_start
  config_summary?: string;
  // write
  after_hash?: string;
  data?: string;
  size?: number;
  // rename
  old_path?: string;
  new_path?: string;
  // http_request / http_response
  method?: string;
  url?: string;
  headers?: unknown;
  body?: string;
  flow_id?: string;
  status?: number;
  // network connection
  remote_addr?: string;
  remote_port?: number;
  domain?: string;
  sock_type?: string;
  [key: string]: unknown;
}

export interface EventsResponse {
  events: ArgusEvent[];
  count: number;
}
