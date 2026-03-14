// Shared TypeScript types for the frontend.
// Add domain types, API response shapes, and utility types here.
// Component-specific prop types should stay co-located with their component.

export type Result<T, E = Error> = { ok: true; value: T } | { ok: false; error: E };

export interface Paginated<T> {
  data: T[];
  meta: {
    page: number;
    perPage: number;
    total: number;
  };
}
