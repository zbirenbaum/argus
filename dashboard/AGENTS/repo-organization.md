# TypeScript Repository Organization Best Practices

Opinionated conventions for Bun + TypeScript, with SolidJS + Astro context.

---

## Monorepo vs Single-Package

**Use a monorepo when:**
- 2+ deployable units (frontend + API, or app + shared library)
- You need to share types, utilities, or UI components without publishing to npm
- You want atomic commits spanning multiple packages

**Stay single-package when:**
- One deployable artifact, no shared consumers
- Small team where monorepo tooling complexity is net-negative

**Rule:** If you have a frontend and a backend, use a monorepo. With Bun workspaces the overhead is minimal.

---

## Bun Workspaces Monorepo

### Root `package.json`

```json
{
  "name": "eden",
  "private": true,
  "workspaces": ["packages/*", "apps/*"],
  "scripts": {
    "dev": "bun run --filter '*' dev",
    "build": "bun run --filter '*' build",
    "test": "bun test",
    "typecheck": "bun run --filter '*' typecheck",
    "lint": "biome check .",
    "ci": "bun run typecheck && bun run lint && bun test"
  },
  "devDependencies": {
    "typescript": "^5.4.0",
    "@biomejs/biome": "^1.9.0"
  }
}
```

### Canonical Folder Tree

```
eden/
  apps/
    web/                    # Astro + SolidJS frontend
      src/
      public/
      astro.config.ts
      tsconfig.json
      package.json
    api/                    # Bun HTTP API (Hono / Elysia)
      src/
      tsconfig.json
      package.json
  packages/
    ui/                     # Shared SolidJS component library
      src/
      tsconfig.json
      package.json
    types/                  # Shared TypeScript types only (no runtime code)
      src/
      tsconfig.json
      package.json
    utils/                  # Shared pure utility functions
      src/
      tsconfig.json
      package.json
  AGENTS/
  CLAUDE.md
  tsconfig.base.json
  biome.json
  package.json
  bun.lockb
```

### Workspace Package `package.json`

```json
{
  "name": "@eden/types",
  "version": "0.0.1",
  "private": true,
  "type": "module",
  "exports": {
    ".": "./src/index.ts",
    "./api": "./src/api.ts"
  }
}
```

With Bun workspaces, point `exports` at `.ts` source files — no build step needed for internal packages.

Consumer `package.json`:
```json
{
  "dependencies": {
    "@eden/types": "workspace:*",
    "@eden/utils": "workspace:*"
  }
}
```

---

## Single-App Folder Structure

```
apps/web/src/
  components/
    ui/              # SolidUI primitives (Button, Input, Modal)
      Button/
        Button.tsx
        Button.test.tsx  # co-located unit test
    features/        # Feature-specific smart components
      auth/
        LoginForm.tsx
        LoginForm.test.tsx
      dashboard/
  layouts/           # Astro layout components
    BaseLayout.astro
    BlogLayout.astro
  pages/             # Astro file-based routes
    index.astro
    blog/
      [slug].astro
    api/
      posts.ts
  lib/               # Domain logic, API clients, service abstractions
    auth.ts
    api.ts
    db.ts
  hooks/             # SolidJS reactive primitives
    createUser.ts
    createCart.ts
  stores/            # Global reactive state (SolidJS stores)
    auth.store.ts
    ui.store.ts
  utils/             # Pure, stateless helper functions, zero framework deps
    date.ts
    string.ts
    cn.ts
  types/             # App-local types not shared with other packages
    api.ts
    routes.ts
  styles/
    global.css
  middleware.ts
  env.d.ts
```

---

## Module Boundary Rules

| Directory | What belongs | What does NOT belong |
|-|-|-|
| `components/ui/` | Dumb presentational components, prop interfaces | Business logic, API calls, store imports |
| `components/features/` | Feature-specific smart components | Importing other feature components |
| `lib/` | API clients, auth, DB, domain logic | UI concerns, store mutations |
| `hooks/` | Reactive wrappers around lib/stores | Raw fetch calls, side-effect-free logic |
| `stores/` | Global reactive state, context providers | Derived values (put in hooks/) |
| `utils/` | Pure functions, zero dependencies | Framework-aware or stateful code |
| `types/` | Type aliases, interfaces | Runtime code of any kind |
| `pages/` | Astro routes only | Reusable utilities (use lib/ instead) |

---

## Path Aliases

Configure in **both** `tsconfig.json` and `astro.config.ts`:

```json
// tsconfig.json
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"],
      "@components/*": ["./src/components/*"],
      "@lib/*": ["./src/lib/*"],
      "@hooks/*": ["./src/hooks/*"],
      "@stores/*": ["./src/stores/*"],
      "@utils/*": ["./src/utils/*"],
      "@types/*": ["./src/types/*"]
    }
  }
}
```

```ts
// astro.config.ts
export default defineConfig({
  vite: {
    resolve: {
      alias: {
        "@": "./src",
        "@components": "./src/components",
        "@lib": "./src/lib",
      },
    },
  },
});
```

**Rules:**
- `@/` is the escape hatch for anything not covered by a specific alias
- Keep aliases shallow — `@components/ui/Button`, not `@components/ui`
- Never alias into `node_modules`

---

## Shared Types Organization

```
packages/types/src/
  index.ts          # Re-exports all public types
  api.ts            # Request/response interfaces shared by frontend + backend
  models.ts         # Domain entity types (User, Post, etc.)
  common.ts         # Utility types (Result<T>, Paginated<T>, etc.)
```

- Cross-package types live in `packages/types/` — imported as `@eden/types`
- App-local types live in `apps/web/src/types/`
- Component prop types are **co-located with the component**, not extracted to types/
- API contract types live in `packages/types/src/api.ts` so frontend and backend share one definition

```ts
// packages/types/src/common.ts
export type Result<T, E = Error> =
  | { ok: true; value: T }
  | { ok: false; error: E };

export interface Paginated<T> {
  data: T[];
  meta: { page: number; perPage: number; total: number };
}
```

---

## Barrel Files (index.ts) — Strict Rules

**Use barrels for:**
- Public API of an entire package (`packages/ui/src/index.ts`)
- `types/index.ts` — single re-export of all domain types
- Feature groups with 3+ files consumed externally

**Never use barrels for:**
- `components/features/` — causes circular dependencies, kills tree-shaking
- `stores/` — import stores directly to keep the dependency graph clear
- Deeply nested directories — adds indirection without benefit
- Blanket re-export of everything in a directory

```ts
// GOOD: packages/ui/src/index.ts — explicit public API
export { Button } from "./components/Button/Button";
export { Modal } from "./components/Modal/Modal";
export type { ButtonProps, ModalProps } from "./types";

// BAD: components/index.ts — re-exports everything
export * from "./Button/Button";
export * from "./Modal/Modal";
// Prevents tree-shaking, creates coupling
```

---

## Test File Organization

**Unit tests:** co-locate with the source file.
```
components/
  Button/
    Button.tsx
    Button.test.tsx    # right next to the component
lib/
  utils.ts
  utils.test.ts
```

**Integration/E2E tests:** top-level `e2e/` directory.
```
apps/web/
  src/                 # unit tests co-located here
  e2e/                 # Playwright E2E tests
    auth.spec.ts
    checkout.spec.ts
```

Bun test runner discovers `*.test.ts` and `*.test.tsx` automatically. Use `.test.ts` — not `.spec.ts` — for unit tests (pick one convention, standardize).

```ts
import { describe, it, expect } from "bun:test";
```

---

## Naming Conventions

| Thing | Convention | Example |
|-|-|-|
| Components | PascalCase | `UserCard.tsx` |
| Hooks/primitives | camelCase with verb prefix | `createUserSession.ts` |
| Stores | camelCase + `.store.ts` | `auth.store.ts` |
| Utilities | camelCase | `formatDate.ts` |
| Type files | camelCase | `api.ts`, `domain.ts` |
| Constants | `SCREAMING_SNAKE_CASE` in code | `MAX_RETRY_COUNT` |
| Astro pages | kebab-case | `user-profile.astro` |
| Route params | bracket notation | `[id].astro` |
| Test files | same name + `.test.ts` | `Button.test.tsx` |

---

## `package.json` Conventions

```json
{
  "name": "@eden/web",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "astro dev",
    "build": "astro build",
    "preview": "astro preview",
    "typecheck": "tsc --noEmit",
    "lint": "biome check .",
    "lint:fix": "biome check --write .",
    "test": "bun test",
    "test:watch": "bun test --watch",
    "clean": "rm -rf dist .astro",
    "ci": "bun run typecheck && bun run lint && bun test"
  }
}
```

**Rules:**
- Always `"type": "module"` — Bun is ESM-first
- Use `workspace:*` for internal package references
- `typecheck` runs separately from `build` — run both in CI
- Never use `npm`/`npx` — use `bun`/`bunx` everywhere
- Commit `bun.lockb` — never delete it

---

## Import Ordering

Enforce with Biome's `organizeImports: true`. Canonical order:

```ts
// 1. Node/Bun built-ins
import { readFile } from "node:fs/promises";

// 2. External packages
import { createSignal } from "solid-js";
import { z } from "zod";

// 3. Internal workspace packages
import type { User } from "@eden/types";
import { formatDate } from "@eden/utils";

// 4. App-level aliases
import { api } from "@lib/api";
import { useUser } from "@hooks/createUser";

// 5. Relative imports
import { helper } from "./helper";
import type { Props } from "./types";
```

`import type` always for type-only imports — `verbatimModuleSyntax` in tsconfig enforces this.

---

## CI/CD

```yaml
# .github/workflows/ci.yml
name: CI
on: [push, pull_request]
jobs:
  ci:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: oven-sh/setup-bun@v2
        with:
          bun-version: latest
      - run: bun install --frozen-lockfile
      - run: bun run typecheck
      - run: bun run lint
      - run: bun test
      - run: bun run build
```

**Rules:**
- `--frozen-lockfile` always in CI
- typecheck → lint → test → build (fail fast)
- `--max-warnings 0` on lint — treat warnings as errors in CI
- Never skip typecheck even if Biome catches many issues — Biome does not do full type checking
