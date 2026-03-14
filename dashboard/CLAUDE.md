# Eden Frontend

## Before Starting Any Task

Read the relevant files in `AGENTS/` — they are the authoritative reference for this stack:

| File | When to read |
|-|-|
| `AGENTS/solidjs.md` | Touching SolidJS components, signals, stores, reactivity |
| `AGENTS/astro.md` | Touching pages, layouts, routing, content, islands |
| `AGENTS/tailwind-solidui.md` | Touching styling, variants, SolidUI components, dark mode |
| `AGENTS/typescript-linting.md` | Configuring TypeScript, Biome, linting, git hooks |
| `AGENTS/repo-organization.md` | Adding files, creating new modules, folder structure |

When in doubt, read all of them — they are concise.

## Stack

- **Runtime/Package Manager:** Bun (`bun` / `bunx` — never `npm` / `npx`)
- **Framework:** Astro 5 (hybrid rendering, file-based routing)
- **UI Islands:** SolidJS via `@astrojs/solid-js`
- **Styling:** Tailwind CSS v4 (CSS-first config in `src/styles/global.css`)
- **Components:** SolidUI (shadcn/ui port) — lives in `src/components/ui/`
- **Linter/Formatter:** Biome (`biome.json`) — runs on pre-commit
- **TypeScript:** Strict mode — see `AGENTS/typescript-linting.md`

## Key Conventions

- SolidJS components go in `src/components/` — never destructure props
- Use `<For>`, `<Show>`, `<Switch>` — never `.map()` for reactive lists
- Fetch data in Astro frontmatter; pass as props to SolidJS islands
- `client:visible` is the default hydration directive — `client:load` only for critical UI
- All class merging via `cn()` from `@utils/cn` — never string concatenation
- Variants via CVA — never ad-hoc ternaries in class strings
- Dark mode via CSS variables — never scatter `dark:` utilities in components
- `import type` always for type-only imports (`verbatimModuleSyntax` enforces this)
- Path aliases: `@/*` → `src/*`, `@components/*`, `@lib/*`, `@hooks/*`, `@stores/*`, `@utils/*`

## Commands

```bash
bun dev          # dev server
bun run build    # production build
bun run typecheck  # type check
bun run lint     # biome lint
bun run lint:fix # biome lint + autofix
bun test         # run tests
bun run ci       # full check (typecheck + lint + test + build)
```

## Rules

1. Never say "the solution requires a large rewrite" — just do it
2. Always prioritize quality, no matter the lift
3. Read AGENTS/ files before implementing anything in that domain
4. Ask if uncertain about scope or requirements
