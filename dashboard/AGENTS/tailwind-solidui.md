# Tailwind CSS v4 + SolidUI Best Practices

---

## Tailwind v4 — What Changed from v3

- **No `tailwind.config.js` by default** — configuration lives in CSS via `@theme`
- **CSS-first config** — use `@import "tailwindcss"` and `@theme { ... }` instead of a JS config
- **No `content` array needed** — v4 auto-detects template files via Vite/Oxide engine
- **CSS variables first-class** — all design tokens emit as `--color-*`, `--spacing-*`, etc.
- **Native cascade layers** — `@layer base`, `@layer components`, `@layer utilities` map to real CSS layers
- **5-10x faster** via Oxide (Rust) engine

---

## Setup with Astro

```bash
bun add -d tailwindcss @tailwindcss/vite
```

```ts
// astro.config.ts
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  vite: {
    plugins: [tailwindcss()],
  },
});
```

```css
/* src/styles/global.css */
@import "tailwindcss";
```

---

## CSS-First Configuration

All design tokens live in `@theme` in a single CSS file. Do not split tokens across files.

```css
/* src/styles/global.css */
@import "tailwindcss";

@theme {
  /* Typography */
  --font-sans: "Inter", ui-sans-serif, system-ui, sans-serif;
  --font-mono: "JetBrains Mono", ui-monospace, monospace;

  /* Brand colors */
  --color-brand-50: oklch(97% 0.02 260);
  --color-brand-500: oklch(55% 0.18 260);
  --color-brand-900: oklch(25% 0.12 260);

  /* Border radius */
  --radius-sm: 0.25rem;
  --radius-md: 0.375rem;
  --radius-lg: 0.5rem;

  /* Shadows */
  --shadow-card: 0 1px 3px oklch(0% 0 0 / 0.12), 0 1px 2px oklch(0% 0 0 / 0.08);
}
```

Tokens defined in `@theme` automatically become Tailwind utilities (`bg-brand-500`, `rounded-lg`, `shadow-card`, etc.) **and** raw CSS custom properties (`var(--color-brand-500)`).

---

## Dark Mode — CSS Variables Approach

Avoid `dark:` utility proliferation. Use semantic CSS variables that flip on `.dark`. Components reference semantic tokens, not raw color scales.

```css
@layer base {
  :root {
    --bg: var(--color-white);
    --bg-subtle: var(--color-neutral-50);
    --fg: var(--color-neutral-900);
    --fg-muted: var(--color-neutral-500);
    --border: var(--color-neutral-200);
    --ring: var(--color-brand-500);
  }

  .dark {
    --bg: var(--color-neutral-950);
    --bg-subtle: var(--color-neutral-900);
    --fg: var(--color-neutral-50);
    --fg-muted: var(--color-neutral-400);
    --border: var(--color-neutral-800);
    --ring: var(--color-brand-400);
  }
}

@theme {
  --color-bg: var(--bg);
  --color-fg: var(--fg);
  --color-fg-muted: var(--fg-muted);
  --color-border: var(--border);
  --color-ring: var(--ring);
}
```

Use `bg-bg`, `text-fg`, `border-border` — never scatter `dark:bg-neutral-950` throughout components.

**Dark mode toggle (SolidJS):**
```tsx
const [dark, setDark] = createSignal(
  document.documentElement.classList.contains("dark")
);

function toggleDark() {
  document.documentElement.classList.toggle("dark");
  setDark(d => !d);
  localStorage.setItem("theme", dark() ? "dark" : "light");
}
```

**Initialize before first paint (in `<head>` to avoid FOUC):**
```astro
<script is:inline>
  const theme = localStorage.getItem("theme") ??
    (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  document.documentElement.classList.toggle("dark", theme === "dark");
</script>
```

---

## Class Organization

Install and enforce the Prettier plugin — it sorts classes automatically:

```bash
bun add -d prettier prettier-plugin-tailwindcss
```

```json
// .prettierrc
{
  "plugins": ["prettier-plugin-tailwindcss"],
  "tailwindFunctions": ["cn", "cva", "clsx"],
  "tailwindStylesheet": "./src/styles/global.css"
}
```

Mental ordering model (Prettier enforces this automatically):
1. Layout (`flex`, `grid`, `block`, `hidden`)
2. Position (`relative`, `absolute`, `inset-*`)
3. Sizing (`w-*`, `h-*`, `min-*`, `max-*`)
4. Spacing (`p-*`, `m-*`, `gap-*`)
5. Typography (`font-*`, `text-*`, `leading-*`)
6. Visual (`bg-*`, `border-*`, `rounded-*`, `shadow-*`, `ring-*`)
7. State (`hover:*`, `focus:*`, `disabled:*`)
8. Responsive (`sm:*`, `md:*`, `lg:*`)

---

## Component Variants with CVA

Use CVA for all variant logic — never string concatenation or template literals.

```bash
bun add class-variance-authority clsx tailwind-merge
```

```ts
// src/utils/cn.ts
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
```

```tsx
// src/components/ui/Button/Button.tsx
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@utils/cn";
import { splitProps, type JSX, type Component } from "solid-js";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 rounded-md font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50",
  {
    variants: {
      variant: {
        default: "bg-brand-500 text-white hover:bg-brand-600",
        outline: "border border-border bg-bg text-fg hover:bg-bg-subtle",
        ghost: "text-fg hover:bg-bg-subtle",
        destructive: "bg-red-600 text-white hover:bg-red-700",
      },
      size: {
        sm: "h-8 px-3 text-sm",
        md: "h-10 px-4 text-sm",
        lg: "h-12 px-6 text-base",
        icon: "h-10 w-10",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "md",
    },
  }
);

interface ButtonProps
  extends JSX.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

const Button: Component<ButtonProps> = (props) => {
  const [local, rest] = splitProps(props, ["variant", "size", "class"]);
  return (
    <button
      class={cn(buttonVariants({ variant: local.variant, size: local.size }), local.class)}
      {...rest}
    />
  );
};

export { Button, buttonVariants };
export type { ButtonProps };
```

**CVA rules:**
- Base classes go in the first argument — never inside a variant
- Variants represent mutually exclusive states
- Use `compoundVariants` for styles that only apply when multiple variants combine
- Export `VariantProps` types; components accept and spread them
- Always accept a `class` prop and append it last (allows external override)

---

## Responsive Design

Mobile-first always. Never write desktop-first overrides with `max-*` breakpoints.

```tsx
// Good
<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">

// Bad — desktop-first
<div class="grid grid-cols-3 gap-4 max-lg:grid-cols-2 max-sm:grid-cols-1">
```

**Standard breakpoints:** `sm` (640px), `md` (768px), `lg` (1024px), `xl` (1280px). Avoid arbitrary breakpoints.

---

## `tailwind.config.ts` — Only When Needed

In v4, use a JS config **only** for plugins and safelisting. Never put tokens here.

```ts
import type { Config } from "tailwindcss";
import typography from "@tailwindcss/typography";

export default {
  plugins: [typography],
  safelist: [
    { pattern: /^bg-(brand|red|green)-(500|600)$/ },
  ],
} satisfies Config;
```

---

## SolidUI Setup

SolidUI is the SolidJS port of shadcn/ui — copy-paste components, not an npm package. You own them.

```bash
bunx astro add solid
bunx shadcn-solid@latest init
bunx shadcn-solid@latest add button dialog input label
```

```json
// components.json
{
  "$schema": "https://solidui.vercel.app/schema.json",
  "style": "default",
  "tailwind": {
    "config": "tailwind.config.ts",
    "css": "src/styles/global.css",
    "baseColor": "neutral",
    "cssVariables": true
  },
  "aliases": {
    "components": "@/components",
    "utils": "@/utils/cn"
  }
}
```

Always use `cssVariables: true`. The raw-color alternative makes dark mode and theming significantly harder.

---

## SolidUI Theming

```css
@layer base {
  :root {
    --background: 0 0% 100%;
    --foreground: 240 10% 3.9%;
    --primary: 240 5.9% 10%;
    --primary-foreground: 0 0% 98%;
    --muted: 240 4.8% 95.9%;
    --muted-foreground: 240 3.8% 46.1%;
    --border: 240 5.9% 90%;
    --ring: 240 5.9% 10%;
    --radius: 0.5rem;
  }
  .dark {
    --background: 240 10% 3.9%;
    --foreground: 0 0% 98%;
    /* ... */
  }
}
```

Map into `@theme` for Tailwind v4:
```css
@theme {
  --color-background: hsl(var(--background));
  --color-foreground: hsl(var(--foreground));
  --color-primary: hsl(var(--primary));
  --color-border: hsl(var(--border));
}
```

To rebrand: edit CSS variables only — never touch component files for theming.

---

## Component Customization

**Add a CVA variant (preferred for reusable additions):**
```tsx
const buttonVariants = cva("...", {
  variants: {
    variant: {
      default: "...",
      brand: "bg-brand-500 text-white hover:bg-brand-600", // added
    },
  },
});
```

**`cn()` for one-off overrides:**
```tsx
<Button class={cn("w-full mt-6", props.class)}>Submit</Button>
```

**Compose, don't wrap:**
```tsx
const SubmitButton = (props: ButtonProps) => (
  <Button type="submit" variant="default" class={cn("w-full", props.class)} {...props} />
);
```

---

## Accessibility

SolidUI uses **Kobalte** (SolidJS's Radix equivalent) — ARIA, keyboard nav, focus trapping, and screen reader announcements are handled automatically.

```tsx
// Icon-only button — always aria-label
<Button size="icon" aria-label="Close dialog">
  <XIcon aria-hidden="true" />
</Button>

// Input with associated label
<Label for="email">Email address</Label>
<Input id="email" type="email" autocomplete="email" />
```

- Never remove focus rings — customize with `ring` utilities, never hide globally
- Let Kobalte manage focus trapping in dialogs — don't manually manage `tabIndex` or `aria-modal`
- Test with keyboard only: Tab, Shift+Tab, Enter, Escape, Arrow keys

---

## SolidUI vs React shadcn/ui — Key Differences

| Concern | SolidUI | React shadcn/ui |
|-|-|-|
| Class prop | `class` | `className` |
| State | `createSignal`, `createMemo` | `useState`, `useMemo` |
| Primitives | Kobalte | Radix UI |
| Docs reference | kobalte.dev | radix-ui.com |
| `children` | Lazy getter, never iterate | Array |
| SSR in Astro | Needs `client:*` directive for interactivity | Same |

---

## Anti-Patterns

| Anti-Pattern | Fix |
|-|-|
| Unsorted class strings | Use `prettier-plugin-tailwindcss` |
| String concatenation for variants | Use CVA |
| Hardcoded colors (`text-blue-500`) | Use semantic variables (`text-primary`) |
| Scattering `dark:` utilities | Use semantic CSS variables |
| `@apply` in component files | Use CVA or inline classes |
| Removing focus rings | Customize with `ring` utilities |
| Tokens in `tailwind.config.ts` (v4) | Move to `@theme` in CSS |
| Design tokens split across multiple files | Consolidate in one `global.css` |
| Wrapping SolidUI components in extra layers | Compose via `cn()` and spread props |
