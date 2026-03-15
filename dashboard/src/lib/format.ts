/** Format an ISO timestamp as HH:MM:SS. */
export function formatTime(tsWall: string): string {
  try {
    const d = new Date(tsWall);
    const h = String(d.getHours()).padStart(2, "0");
    const m = String(d.getMinutes()).padStart(2, "0");
    const s = String(d.getSeconds()).padStart(2, "0");
    return `${h}:${m}:${s}`;
  } catch {
    return tsWall;
  }
}

/** Format an ISO timestamp as HH:MM:SS.mmm (with milliseconds). */
export function formatTimeMs(tsWall: string): string {
  try {
    const d = new Date(tsWall);
    const h = String(d.getHours()).padStart(2, "0");
    const m = String(d.getMinutes()).padStart(2, "0");
    const s = String(d.getSeconds()).padStart(2, "0");
    const ms = String(d.getMilliseconds()).padStart(3, "0");
    return `${h}:${m}:${s}.${ms}`;
  } catch {
    return tsWall;
  }
}
