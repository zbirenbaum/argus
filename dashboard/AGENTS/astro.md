# Astro Framework Best Practices 2024–2025

---

## Core Mental Model

Astro renders everything to static HTML by default. Islands are opt-in interactive components hydrated on the client. This is the fundamental mental model — ship zero JS unless explicitly opted in.

---

## Island Architecture — Hydration Directives

Choosing the right hydration directive is the single biggest performance lever.

| Directive | When it hydrates | Use case |
|-|-|-|
| `client:load` | Immediately on page load | Critical UI: nav dropdowns, auth modals, above-the-fold |
| `client:idle` | After `requestIdleCallback` | Secondary UI: sidebars, non-critical widgets |
| `client:visible` | When element enters viewport | Below-fold: comments, carousels, charts |
| `client:media="(max-width: 768px)"` | When media query matches | Mobile-only menus, responsive-only widgets |
| `client:only="solid-js"` | Client-only, no SSR render | Browser-only APIs (WebGL, canvas, `window.*`) |

```astro
---
import Counter from "@components/solid/Counter";
import HeavyChart from "@components/solid/HeavyChart";
import MobileMenu from "@components/solid/MobileMenu";
---

<Counter client:load />         <!-- Critical: ready immediately -->
<HeavyChart client:visible />   <!-- Below fold: lazy hydrate -->
<MobileMenu client:only="solid-js" />  <!-- Uses window, cannot SSR -->
```

**Rules:**
- Default to `client:visible`. Upgrade to `client:load` only when interaction must be ready before scroll.
- Prefer `client:idle` over `client:load` for anything not needed in the first 2 seconds.
- `client:only` loses SSR content — hurts SEO and LCP. Use sparingly.
- Pass data to islands via props, not DOM queries. Islands are isolated.
- Keep island components small. A large island defeats partial hydration.

---

## SolidJS Integration

```bash
bunx astro add solid
```

```ts
// astro.config.ts
import { defineConfig } from "astro/config";
import solid from "@astrojs/solid-js";

export default defineConfig({
  integrations: [solid()],
});
```

If mixing frameworks, scope with `include`:
```ts
integrations: [
  solid({ include: ["**/solid/**"] }),
  react({ include: ["**/react/**"] }),
],
```

**Pass static data from Astro as props; derive reactive state inside SolidJS:**
```tsx
// Good: server fetches, SolidJS handles interactivity
export default function UserCard({ user }: { user: SerializedUser }) {
  const [following, setFollowing] = createSignal(user.isFollowing);
  // ...
}
```

**Shared island state** — use module-level stores (not per-component):
```ts
// src/stores/cart.ts
import { createStore } from "solid-js/store";
export const [cart, setCart] = createStore<{ items: CartItem[] }>({ items: [] });
```

---

## Rendering Modes

```ts
// astro.config.ts
export default defineConfig({
  output: "static",   // Default — full SSG, CDN delivery
  // output: "server",   // Full SSR — requires adapter
  // output: "hybrid",   // Per-page choice — most flexible
});
```

| Scenario | Recommended |
|-|-|
| Blog, docs, marketing | `static` |
| Dashboard, user-specific content | `server` |
| Mostly static + a few dynamic routes | `hybrid` |
| E-commerce (product pages static, cart dynamic) | `hybrid` |

**Hybrid mode — opt pages in/out per file:**
```astro
---
// SSR page (in hybrid mode)
export const prerender = false;

const user = await getUser(Astro.cookies.get("session")?.value);
if (!user) return Astro.redirect("/login");
---
```

```astro
---
// Static page (in server mode)
export const prerender = true;
---
```

**Always install an adapter for SSR/hybrid:**
```bash
bunx astro add vercel      # Vercel
bunx astro add cloudflare  # Cloudflare Workers
bunx astro add node        # Self-hosted Node.js
```

---

## Content Collections

Use Content Collections for all structured content. Never use raw `import.meta.glob`.

```ts
// src/content/config.ts
import { defineCollection, z, reference } from "astro:content";

const blog = defineCollection({
  type: "content",
  schema: ({ image }) => z.object({
    title: z.string(),
    description: z.string().max(160),
    pubDate: z.coerce.date(),
    updatedDate: z.coerce.date().optional(),
    author: reference("authors"),
    tags: z.array(z.string()).default([]),
    cover: image().optional(), // use image() helper, not z.string()
    draft: z.boolean().default(false),
  }),
});

const authors = defineCollection({
  type: "data", // .json / .yaml files
  schema: z.object({
    name: z.string(),
    bio: z.string(),
    avatar: z.string().url(),
  }),
});

export const collections = { blog, authors };
```

**Querying:**
```ts
import { getCollection, getEntry, render } from "astro:content";

// All published posts, sorted
const posts = (await getCollection("blog", ({ data }) => !data.draft))
  .sort((a, b) => b.data.pubDate.valueOf() - a.data.pubDate.valueOf());

// Single entry
const post = await getEntry("blog", Astro.params.slug);
if (!post) return Astro.redirect("/404");

const { Content, headings } = await render(post);
```

**Dynamic routes with collections:**
```astro
---
import { getCollection, render } from "astro:content";
import Layout from "@layouts/Layout.astro";

export async function getStaticPaths() {
  const posts = await getCollection("blog", ({ data }) => !data.draft);
  return posts.map(post => ({
    params: { slug: post.id },
    props: { post },
  }));
}

const { post } = Astro.props;
const { Content } = await render(post);
---
<Layout title={post.data.title}>
  <Content />
</Layout>
```

**Rules:**
- Always define a schema — skipping it loses all type safety on `data.*`
- Use `z.coerce.date()` for dates (handles both strings and Date objects)
- Use `image()` helper for image fields — enables `<Image />` optimization
- Use `reference()` for relational data (authors, categories)
- Put reference collections in `type: "data"` collections

---

## File-Based Routing

```
src/pages/
  index.astro              → /
  about.astro              → /about
  blog/
    index.astro            → /blog
    [slug].astro           → /blog/:slug
    [...slug].astro        → /blog/* (catch-all)
  api/
    posts.ts               → /api/posts
  404.astro                → Custom 404
```

**Route priority:** static > dynamic > catch-all. `blog/featured.astro` always wins over `blog/[slug].astro`.

**API endpoints:**
```ts
// src/pages/api/posts.ts
import type { APIRoute } from "astro";

export const GET: APIRoute = async ({ url }) => {
  const tag = url.searchParams.get("tag");
  const posts = await getCollection("blog");
  return Response.json(posts.map(p => ({ slug: p.id, title: p.data.title })));
};

export const POST: APIRoute = async ({ request }) => {
  const body = await request.json();
  return Response.json({ ok: true }, { status: 201 });
};
```

In hybrid/server mode, API routes should always have `export const prerender = false`.

---

## Layouts

```astro
---
// src/layouts/Layout.astro
interface Props {
  title: string;
  description?: string;
}
const { title, description = "Default description" } = Astro.props;
---
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width" />
    <meta name="description" content={description} />
    <title>{title}</title>
  </head>
  <body>
    <slot />
  </body>
</html>
```

Named slots for flexible regions:
```astro
<header><slot name="header" /></header>
<main><slot /></main>
<aside><slot name="sidebar" /></aside>
```

---

## Image Optimization

Always use `<Image />` — never raw `<img>` for local assets.

```astro
---
import { Image, Picture } from "astro:assets";
import hero from "@/assets/hero.jpg";
---

<!-- Standard optimized image -->
<Image
  src={hero}
  alt="Hero banner"
  width={1200}
  height={630}
  format="webp"
  quality={80}
  loading="eager"
  fetchpriority="high"
/>

<!-- Art direction with multiple formats -->
<Picture
  src={hero}
  formats={["avif", "webp"]}
  widths={[400, 800, 1200]}
  sizes="(max-width: 800px) 100vw, 1200px"
  alt="Hero"
/>
```

**Dynamic/programmatic use:**
```ts
import { getImage } from "astro:assets";
import base from "@/assets/og-base.png";

const og = await getImage({ src: base, format: "png", width: 1200, height: 630 });
// og.src → optimized URL
```

**Configure remote images:**
```ts
export default defineConfig({
  image: {
    domains: ["cdn.example.com"],
    remotePatterns: [{ protocol: "https", hostname: "**.cloudinary.com" }],
  },
});
```

**Rules:**
- Store source images in `src/assets/` (processed), NOT `public/` (pass-through)
- `loading="eager"` + `fetchpriority="high"` on the LCP image only
- `formats={["avif", "webp"]}` for best modern format support with fallback

---

## Environment Variables

Use `astro:env` (Astro 5+) for type-safe, validated env vars:

```ts
// astro.config.ts
import { defineConfig, envField } from "astro/config";

export default defineConfig({
  env: {
    schema: {
      DATABASE_URL: envField.string({ context: "server", access: "secret" }),
      PUBLIC_SITE_URL: envField.string({ context: "client", access: "public" }),
      API_TIMEOUT_MS: envField.number({ context: "server", access: "secret", default: 5000 }),
    },
  },
});
```

```ts
import { DATABASE_URL } from "astro:env/server";
import { PUBLIC_SITE_URL } from "astro:env/client";
```

Astro validates at build time — missing required vars fail the build.

**Astro 4 fallback:** `import.meta.env.VARIABLE` (no build-time validation).

---

## Middleware

```ts
// src/middleware.ts
import { defineMiddleware, sequence } from "astro:middleware";

const auth = defineMiddleware(async ({ cookies, url, redirect, locals }, next) => {
  const session = cookies.get("session")?.value;
  const user = session ? await validateSession(session) : null;
  locals.user = user;

  const protectedRoutes = ["/dashboard", "/settings"];
  if (protectedRoutes.some(r => url.pathname.startsWith(r)) && !user) {
    return redirect("/login?next=" + url.pathname);
  }
  return next();
});

const logger = defineMiddleware(async ({ url, request }, next) => {
  const start = Date.now();
  const response = await next();
  console.log(`${request.method} ${url.pathname} — ${Date.now() - start}ms`);
  return response;
});

export const onRequest = sequence(logger, auth);
```

Type `locals` in `src/env.d.ts`:
```ts
/// <reference types="astro/client" />
declare namespace App {
  interface Locals {
    user: import("./lib/auth").User | null;
  }
}
```

**Middleware only runs on SSR/hybrid pages — not on prerendered static pages.**

---

## Performance: Prefetch + View Transitions

```ts
// astro.config.ts
export default defineConfig({
  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover", // "hover" | "tap" | "viewport" | "load"
  },
});
```

Per-link overrides:
```html
<a href="/heavy" data-astro-prefetch="viewport">Lazy prefetch</a>
<a href="/skip" data-astro-prefetch="false">No prefetch</a>
```

**View Transitions:**
```astro
---
import { ViewTransitions } from "astro:transitions";
---
<head>
  <ViewTransitions />
</head>

<!-- Persistent across navigations -->
<audio src={track} transition:persist />

<!-- Named transition target -->
<h1 transition:name="page-title" transition:animate="slide" />
```

Client-side navigation lifecycle:
```astro
<script>
  document.addEventListener("astro:page-load", () => {
    // Runs on every navigation, including client-side
    initAnalytics();
  });
</script>
```

---

## Anti-Patterns

| Anti-Pattern | Fix |
|-|-|
| `client:load` on everything | Default to `client:visible` / `client:idle` |
| Giant monolithic islands | Split into smaller focused islands |
| Fetching data inside islands | Fetch in Astro frontmatter, pass as props |
| Non-serializable island props | Only pass JSON-serializable values |
| `import.meta.glob` for content | Use Content Collections |
| Dynamic routes without `getStaticPaths` in static mode | Add `getStaticPaths` |
| `<img>` for local assets | Use `<Image />` from `astro:assets` |
| `loading="eager"` on all images | Eager only on the LCP image |
| Heavy computation in middleware | Compute at build time; middleware = auth + routing only |
| Server-only code imported in islands | Keep DB clients in `.astro` or server-only utils |
| Skipping `src/env.d.ts` | Declare `App.Locals`, env types for full type safety |
| Utilities in `src/pages/` | Move to `src/lib/` |
