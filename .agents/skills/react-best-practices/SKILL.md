---
name: react-best-practices
description: React and Next.js performance guidelines from Vercel Engineering. Use when writing, reviewing, or refactoring React components, hooks, client data fetching, rendering behavior, bundle imports, async request flows, or Next.js pages and server code.
license: MIT
metadata:
  source: "https://github.com/vercel-labs/agent-skills/tree/main/skills/react-best-practices"
  source_commit: "180115660cfb8a86b808f117475a01f54caf3bc5"
  upstream_name: "vercel-react-best-practices"
  version: "1.0.0"
---

# React Best Practices

Use this skill to apply Vercel Engineering's React and Next.js performance rules without loading the full 70-rule guide up front.

For this repository, treat React 19 + Vite + Tauri as the default app context. Next.js, RSC, and server-action rules apply only when editing Next.js code or reference projects.

## Workflow

1. Classify the task by performance surface.
2. Read only the matching rule files under `references/rules`.
3. Apply the smallest code change that matches the rule and the local code style.
4. Verify with the nearest test, typecheck, build, or browser check.

Do not apply rules mechanically. Prefer evidence from the current component, data flow, and user interaction path.

## Rule Groups

Read `references/rules/_sections.md` when you need the category index.

| Priority | Group | Read When |
| --- | --- | --- |
| 1 | `async-*` | Removing async waterfalls, parallelizing independent work, delaying awaits until needed |
| 2 | `bundle-*` | Reducing client bundle cost, dynamic imports, direct imports, third-party loading |
| 3 | `server-*` | Next.js server code, RSC serialization, server caching, server fetch parallelism |
| 4 | `client-*` | Client data fetching, SWR-style dedupe, browser storage, global event listeners |
| 5 | `rerender-*` | Hooks, props, memoization, derived state, callback stability, frequent updates |
| 6 | `rendering-*` | DOM/rendering cost, hydration behavior, content visibility, script/resource hints |
| 7 | `js-*` | Hot JavaScript paths, repeated lookups, loops, storage reads, layout thrashing |
| 8 | `advanced-*` | Effect events, latest refs, app initialization, stable callback refs |

## High-Signal Defaults

- Start with `async-*` and `bundle-*` when performance problems are broad or user-visible.
- For React component changes, check `rerender-derived-state-no-effect.md`, `rerender-move-effect-to-event.md`, and `rerender-no-inline-components.md` early.
- Use event handlers for user actions. Use `useEffect` only to synchronize with external systems.
- Avoid adding `useMemo` or `memo` for cheap primitive calculations. Read `rerender-simple-expression-in-memo.md` first.
- For lists and repeated lookups, check `js-index-maps.md`, `js-set-map-lookups.md`, and `rendering-content-visibility.md`.
- For this Tauri/Vite app, skip Next.js-only fixes unless the edited file is in a Next.js reference or the user asks for Next.js guidance.

## Source Layout

Detailed rules are copied from Vercel's `agent-skills` repository into:

```text
references/rules/<rule-name>.md
```

Each rule file contains rationale, incorrect examples, correct examples, and caveats. Load the specific rule file before making a non-trivial React performance change.
