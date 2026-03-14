# SolidJS Best Practices 2024–2025

## Core Mental Model

SolidJS does **not** use a Virtual DOM or component re-renders. Components are functions that run **once** to set up the reactive graph. Updates happen at the signal level — only the DOM nodes that depend on a changed signal update.

- React: re-runs the whole component on state change
- SolidJS: runs the component once; reactive primitives update independently

This is the single most important mental model shift from React.

---

## Signals

```ts
import { createSignal } from "solid-js";

const [count, setCount] = createSignal(0);

// Read: always call as a function inside reactive contexts
<div>{count()}</div>

// Write
setCount(c => c + 1);
```

**Rules:**
- Always read signals inside JSX or reactive contexts (`createEffect`, `createMemo`)
- Reading `count()` in a plain function gives a static snapshot — not reactive
- Use `createMemo` for derived values, never a signal + effect combo

```ts
// WRONG
const [doubled, setDoubled] = createSignal(0);
createEffect(() => setDoubled(count() * 2));

// RIGHT
const doubled = createMemo(() => count() * 2);
```

---

## Stores

Use `createStore` for nested/structured state. Stores use Proxy-based fine-grained tracking.

```ts
import { createStore, produce } from "solid-js/store";

const [state, setState] = createStore({
  user: { name: "Alice", age: 30 },
  items: [{ id: 1, done: false }],
});

// Path-based update (preferred)
setState("user", "name", "Bob");

// Array mutation with produce (immer-style)
setState("items", produce(items => { items[0].done = true; }));
```

**Rule:** signals for scalar/primitive state, stores for objects and arrays.

---

## Effects

```ts
import { createEffect, on } from "solid-js";

// Implicit dependency tracking
createEffect(() => {
  console.log("name:", name());
});

// Explicit dependencies + deferred initial run
createEffect(on(name, (val, prev) => {
  console.log(`${prev} -> ${val}`);
}, { defer: true }));
```

**Rules:**
- Never set signals inside `createEffect` without a guard — causes infinite loops
- Use `createEffect` for side effects only (DOM manipulation, localStorage, logging)
- Always pair subscriptions with `onCleanup`:

```ts
createEffect(() => {
  const id = setInterval(() => tick(), 1000);
  onCleanup(() => clearInterval(id));
});
```

---

## Component Composition

**Never destructure props** — this breaks reactivity.

```tsx
// WRONG — kills reactivity
function Greeting({ name }: { name: string }) { ... }

// RIGHT
function Greeting(props: { name: string }) {
  return <div>Hello {props.name}</div>;
}
```

Use `splitProps` and `mergeProps` for prop manipulation:

```tsx
import { splitProps, mergeProps } from "solid-js";

function Button(props: { label?: string; class?: string; onClick?: () => void }) {
  const merged = mergeProps({ label: "Click me" }, props);
  const [local, rest] = splitProps(merged, ["label", "class"]);
  return (
    <button class={local.class} {...rest}>
      {local.label}
    </button>
  );
}
```

---

## Control Flow

Use built-in control flow components — they are reactive-aware and optimized. **Do not use `.map()` directly for lists.**

```tsx
import { Show, For, Switch, Match, Index } from "solid-js";

// Conditional
<Show when={isLoggedIn()} fallback={<Login />}>
  <Dashboard />
</Show>

// Lists of objects (stable identity)
<For each={items()}>
  {(item) => <li>{item.name}</li>}
</For>

// Lists of primitives (Index is more efficient)
<Index each={numbers()}>
  {(num, i) => <span>{num()}</span>}
</Index>

// Multi-branch
<Switch fallback={<NotFound />}>
  <Match when={route() === "home"}><Home /></Match>
  <Match when={route() === "about"}><About /></Match>
</Switch>
```

`For` re-creates DOM when items change by identity. `Index` recycles DOM nodes — use for primitive arrays or when items lack stable keys.

---

## Performance

### Lazy loading
```tsx
import { lazy, Suspense } from "solid-js";

const HeavyChart = lazy(() => import("./HeavyChart"));

<Suspense fallback={<Spinner />}>
  <HeavyChart />
</Suspense>
```

### Async data with `createResource`
```tsx
const [user, { refetch }] = createResource(userId, async (id) => {
  const res = await fetch(`/api/users/${id}`);
  return res.json();
});

<Show when={!user.loading} fallback={<Spinner />}>
  <UserCard user={user()} />
</Show>
```

### `untrack` and `batch`
```ts
import { untrack, batch } from "solid-js";

// Read without subscribing
createEffect(() => {
  const val = trackedSignal();
  const snapshot = untrack(() => otherSignal());
});

// Group updates into one flush
batch(() => {
  setName("Bob");
  setAge(25);
  setLoading(false);
});
```

---

## Routing (@solidjs/router 0.13+)

```tsx
import { Router, Route } from "@solidjs/router";
import { lazy } from "solid-js";

const Home = lazy(() => import("./pages/Home"));
const UserDetail = lazy(() => import("./pages/UserDetail"));

export default function App() {
  return (
    <Router>
      <Route path="/" component={Home} />
      <Route path="/users/:id" component={UserDetail} />
    </Router>
  );
}
```

```tsx
import { useParams, useNavigate } from "@solidjs/router";

function UserDetail() {
  const params = useParams(); // params.id is reactive
  const navigate = useNavigate();
  const [user] = createResource(() => params.id, fetchUser);

  return (
    <Show when={user()}>
      <h1>{user().name}</h1>
      <button onClick={() => navigate("/")}>Back</button>
    </Show>
  );
}
```

---

## SSR with SolidStart 1.x

File-based routing:
```
src/
  routes/
    index.tsx        → /
    about.tsx        → /about
    users/
      [id].tsx       → /users/:id
```

Server functions with `"use server"`:
```tsx
import { query, createAsync } from "@solidjs/router";

const getUser = query(async (id: string) => {
  "use server";
  return db.users.findById(id);
}, "user");

export default function UserPage() {
  const params = useParams();
  const user = createAsync(() => getUser(params.id));
  return <Show when={user()}><h1>{user().name}</h1></Show>;
}
```

Use `createAsync` in SolidStart routes (SSR-aware). Use `createResource` in client-only SPA contexts.

---

## State Management

### Module-level stores (preferred for global state)
```ts
// store/auth.ts
import { createStore } from "solid-js/store";

const [auth, setAuth] = createStore({
  user: null as User | null,
  token: null as string | null,
});

export const login = (user: User, token: string) => setAuth({ user, token });
export const logout = () => setAuth({ user: null, token: null });
export { auth };
```

No Context needed for truly global state. Use Context for dependency injection or multiple isolated instances.

### Context
```tsx
const ThemeContext = createContext<{ theme: () => string; toggle: () => void }>();

export function ThemeProvider(props: { children: JSX.Element }) {
  const [theme, setTheme] = createSignal("light");
  const toggle = () => setTheme(t => t === "light" ? "dark" : "light");
  return (
    <ThemeContext.Provider value={{ theme, toggle }}>
      {props.children}
    </ThemeContext.Provider>
  );
}

export const useTheme = () => useContext(ThemeContext)!;
```

---

## Common Anti-Patterns

| Anti-Pattern | Fix |
|-|-|
| Destructuring props | Use `props.name` directly |
| `.map()` for reactive lists | Use `<For>` |
| `createEffect` for derived state | Use `createMemo` |
| Signals/stores inside effects | Create at component top level only |
| Missing `onCleanup` | Always cleanup subscriptions/intervals |
| Reading signal outside reactive context | Read inside JSX or reactive primitive |

---

## File Organization

```
src/
  routes/          # SolidStart pages (or pages/ for SPA)
  components/
    ui/            # Dumb primitives (Button, Modal, Input)
    features/      # Feature-specific smart components
  stores/          # Module-level createStore exports
  lib/             # Utilities, API clients, helpers
  hooks/           # createResource wrappers, custom primitives
  types/           # Shared TypeScript interfaces
```

**Naming:** `PascalCase.tsx` for components, `camelCase.ts` for hooks/stores.

**Co-location rule:** keep a component's store/hook/types alongside it until shared, then promote to top-level.
