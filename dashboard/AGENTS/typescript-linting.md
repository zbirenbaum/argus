# Strict TypeScript Linting & Compiler Settings with Bun

Opinionated, maximally strict configuration. Goal: catch bugs at compile time, enforce consistency, eliminate entire categories of runtime errors.

---

## tsconfig.json — Complete Strict Configuration

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ESNext", "DOM", "DOM.Iterable"],
    "outDir": "./dist",
    "rootDir": "./src",
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,

    // Core strictness
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noImplicitReturns": true,
    "noFallthroughCasesInSwitch": true,
    "noPropertyAccessFromIndexSignature": true,
    "noImplicitOverride": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "allowUnreachableCode": false,
    "allowUnusedLabels": false,

    // Module hygiene
    "verbatimModuleSyntax": true,
    "isolatedModules": true,
    "erasableSyntaxOnly": true,
    "forceConsistentCasingInFileNames": true,
    "esModuleInterop": false,
    "allowSyntheticDefaultImports": false,

    // Path aliases
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"],
      "@components/*": ["./src/components/*"],
      "@lib/*": ["./src/lib/*"],
      "@types/*": ["./src/types/*"]
    },

    "resolveJsonModule": true,
    "skipLibCheck": false,
    "useDefineForClassFields": true,
    "types": ["bun-types"]
  },
  "include": ["src/**/*", "tests/**/*"],
  "exclude": ["node_modules", "dist"]
}
```

### Key settings explained

| Setting | Why |
|-|-|
| `noUncheckedIndexedAccess` | `arr[0]` returns `T \| undefined`, not `T` — forces null checks |
| `exactOptionalPropertyTypes` | `{ foo?: string }` rejects `{ foo: undefined }` — stricter optional handling |
| `verbatimModuleSyntax` | Enforces `import type` for type-only imports; required for Bun/ESM |
| `isolatedModules` | Each file must be independently compilable; Bun transpiles per-file |
| `erasableSyntaxOnly` | Bans TypeScript-only syntax that emits JS (enums, namespaces) |
| `noPropertyAccessFromIndexSignature` | Forces bracket notation for index-signature types |
| `skipLibCheck: false` | Checks `.d.ts` files — catches broken type packages early |

---

## Biome — Primary Linter/Formatter (Recommended)

Biome is the recommended choice for Bun projects: single binary, zero config needed for basics, dramatically faster than ESLint + Prettier.

```bash
bun add -d @biomejs/biome
bunx biome init
```

```json
{
  "$schema": "https://biomejs.dev/schemas/1.9.0/schema.json",
  "organizeImports": { "enabled": true },
  "formatter": {
    "enabled": true,
    "indentStyle": "space",
    "indentWidth": 2,
    "lineEnding": "lf",
    "lineWidth": 100
  },
  "linter": {
    "enabled": true,
    "rules": {
      "recommended": true,
      "correctness": {
        "noUnusedVariables": "error",
        "noUnusedImports": "error",
        "useExhaustiveDependencies": "error",
        "noConstantCondition": "error",
        "noUnsafeOptionalChaining": "error"
      },
      "suspicious": {
        "noExplicitAny": "error",
        "noConsole": "warn",
        "noDebugger": "error",
        "noDoubleEquals": "error",
        "noFallthroughSwitchClause": "error",
        "noEmptyInterface": "error",
        "noArrayIndexKey": "warn"
      },
      "style": {
        "noNonNullAssertion": "warn",
        "useConst": "error",
        "useTemplate": "error",
        "noVar": "error",
        "noParameterAssign": "error",
        "useNodejsImportProtocol": "error",
        "useImportType": "error"
      },
      "complexity": {
        "noBannedTypes": "error",
        "noExcessiveCognitiveComplexity": {
          "level": "error",
          "options": { "maxAllowedComplexity": 15 }
        },
        "noForEach": "warn"
      },
      "performance": {
        "noAccumulatingSpread": "error"
      },
      "security": {
        "noGlobalEval": "error"
      }
    }
  },
  "javascript": {
    "formatter": {
      "quoteStyle": "double",
      "semicolons": "always",
      "trailingCommas": "all",
      "arrowParentheses": "always"
    }
  },
  "files": {
    "ignore": ["node_modules", "dist", "**/*.gen.ts", ".astro"]
  }
}
```

---

## ESLint v9 (Alternative — use if Biome lacks needed rules)

```bash
bun add -d eslint @typescript-eslint/eslint-plugin @typescript-eslint/parser
```

```js
// eslint.config.js
import tseslint from "@typescript-eslint/eslint-plugin";
import tsParser from "@typescript-eslint/parser";

export default [
  {
    files: ["**/*.ts", "**/*.tsx"],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        project: "./tsconfig.json",
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: { "@typescript-eslint": tseslint },
    rules: {
      ...tseslint.configs["strict-type-checked"].rules,
      ...tseslint.configs["stylistic-type-checked"].rules,
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/no-unsafe-assignment": "error",
      "@typescript-eslint/no-unsafe-call": "error",
      "@typescript-eslint/no-unsafe-member-access": "error",
      "@typescript-eslint/no-unsafe-return": "error",
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/await-thenable": "error",
      "@typescript-eslint/consistent-type-imports": ["error", { prefer: "type-imports" }],
      "@typescript-eslint/no-import-type-side-effects": "error",
      "@typescript-eslint/no-non-null-assertion": "warn",
      "@typescript-eslint/switch-exhaustiveness-check": "error",
      "@typescript-eslint/prefer-nullish-coalescing": "error",
      "@typescript-eslint/prefer-optional-chain": "error",
      "no-console": "warn",
      "eqeqeq": ["error", "always"],
    },
  },
];
```

---

## Git Hooks with Bun

```bash
bun add -d lint-staged simple-git-hooks
```

```json
// package.json
{
  "scripts": {
    "typecheck": "tsc --noEmit",
    "lint": "biome check .",
    "lint:fix": "biome check --write .",
    "format": "biome format --write .",
    "prepare": "bun simple-git-hooks"
  },
  "simple-git-hooks": {
    "pre-commit": "bunx lint-staged"
  },
  "lint-staged": {
    "*.{ts,tsx,js,jsx}": [
      "biome check --write --no-errors-on-unmatched"
    ],
    "*.{json,css,md}": [
      "biome format --write --no-errors-on-unmatched"
    ]
  }
}
```

Run `bun prepare` after install to register hooks.

---

## Type Safety Patterns

### Avoid `any` — use `unknown`
```ts
// WRONG
function parse(data: any) { return data.name; }

// RIGHT
function parse(data: unknown): string {
  if (typeof data === "object" && data !== null && "name" in data) {
    return String((data as { name: unknown }).name);
  }
  throw new Error("Invalid data shape");
}
```

### Exhaustive switch checks
```ts
type Shape = { kind: "circle"; r: number } | { kind: "square"; side: number };

function area(s: Shape): number {
  switch (s.kind) {
    case "circle": return Math.PI * s.r ** 2;
    case "square": return s.side ** 2;
    default: {
      const _exhaustive: never = s; // compile error if case missed
      return _exhaustive;
    }
  }
}
```

### Opaque/branded types for domain safety
```ts
type UserId = string & { readonly __brand: "UserId" };
type PostId = string & { readonly __brand: "PostId" };

function createUserId(id: string): UserId { return id as UserId; }

function getUser(id: UserId): User { ... }
// getUser(postId) → compile error!
```

### `noUncheckedIndexedAccess` patterns
```ts
const arr = [1, 2, 3];
const first = arr[0]; // type: number | undefined

// Must guard:
if (first !== undefined) {
  console.log(first * 2);
}

// Or assert (use sparingly):
const first = arr[0]!; // non-null assertion — only when you're certain
```

---

## Recommended `package.json` Scripts

```json
{
  "scripts": {
    "dev": "bun run --hot src/index.ts",
    "build": "bun build src/index.ts --outdir dist --target bun",
    "typecheck": "tsc --noEmit",
    "lint": "biome check .",
    "lint:fix": "biome check --write .",
    "test": "bun test",
    "test:watch": "bun test --watch",
    "ci": "bun run typecheck && bun run lint && bun test"
  }
}
```

---

## CI Integration

Run these in order — fail fast:
1. `bun run typecheck` — TypeScript errors
2. `bun run lint` — Biome lint/format
3. `bun test` — Tests

Never skip `typecheck` in CI even if using Biome — Biome does not do full type checking.
