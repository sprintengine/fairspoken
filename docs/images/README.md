# README images

| Image | Files | Source |
| --- | --- | --- |
| Network hero, animated | `network-{light,dark}.svg` (+ static `.png` fallbacks) | `src/network.mjs` |
| App: model picker + live dictation | `app-dictation-{light,dark}.png` | `src/app-showcase.html` (real `home.html`, `index.html`, `cursor-preview.html`) |
| Host dashboard | `host-dashboard-{light,dark}.png` (dashboard is dark-only; the variant is the backdrop) | `src/host-showcase.html` + `src/host-mock.mjs` (real `src-tauri/src/host/dashboard.html`) |

The shared backdrop and window chrome live in `src/showcase.css`, and the palette matches
`src/design-system/tokens.css`. `src/tauri-stub.js` lets the real frontend run
in a plain browser.

## Regenerate

```bash
npm i --no-save playwright-core        # once, at the repo root (uses installed Google Chrome)
node docs/images/src/render.mjs        # everything, or: network | app | host
```

The script starts Vite and the mock host itself. It renders at 2x and runs
`pngquant` and `oxipng` when they're on `PATH` (`brew install pngquant oxipng`).
The network SVGs are generated output, so edit `src/network.mjs`, not the `.svg` files.

## Product name

The name appears in exactly one place per source:

- `src/network.mjs`: `PRODUCT_NAME` (emitted as `<tspan id="product-name">` in each SVG).
- App screenshot: none in `src/`. The name comes from the app itself (`home.html`, "… Cloud" location option).
- Dashboard screenshot: none in `src/`. The name comes from the `brand-name` wordmark in `src-tauri/src/host/dashboard.html`.

To preview a candidate name without touching any app source, run
`PRODUCT_NAME=Foo node docs/images/src/render.mjs`. This regenerates the SVGs
and swaps the visible name in the rendered screenshots.

## Notes

- The SVGs embed Inter (SIL OFL 1.1, from `@fontsource/inter`) as base64. They
  contain only inline CSS animation: no scripts and no external requests, so they
  animate inside GitHub's `<img>` sandbox. They respect `prefers-reduced-motion`.
- The dashboard data is a fictional GP-practice scenario. The browser clock is frozen to
  `SCENARIO_NOW` in `src/host-mock.mjs`.
