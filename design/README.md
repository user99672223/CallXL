# CallXL design tokens

`tokens.json` is the single source of truth for the look of both apps (CLAUDE.md §9).
Keep it flat and boring; add a key to both colour variants at once.

| Group | Keys | Unit |
|---|---|---|
| `color.dark`, `color.light` | `accent`, `on_accent`, `bg`, `surface`, `surface_raised`, `border`, `text`, `text_muted`, `danger`, `success`, `speaking_glow` | `#RRGGBB` or `#RRGGBBAA` |
| `space` | `xxs` 2 … `xxxl` 48 | points / logical px |
| `radius` | `sm`, `md`, `lg`, `xl`, `pill` | points / logical px |
| `type` | `caption`, `body`, `body_strong`, `subtitle`, `title`, `headline` → `{size, weight}` | points, CSS weight |
| `motion` | `fast`, `normal`, `slow`, `glow` | milliseconds |
| `font` | `family` (Inter), `files` (Latin subset TTFs in `fonts/`) | — |
| `size` | `tap_target` 44, `tile`, `sidebar`, `drawer` | points / logical px |

Dark is the default theme; light is functional. There is exactly one accent colour.
`speaking_glow` is the accent with alpha, drawn as a ring/shadow on participant tiles.

## How each UI consumes it

- **Desktop (Slint)** — `apps/desktop/build.rs` reads `tokens.json` at build time and
  generates the `@tokens` Slint library (globals `Colors`, `Space`, `Radius`, `Type`,
  `Motion`, `Size`; snake_case keys become kebab-case, e.g. `surface_raised` →
  `Colors.surface-raised`). `Colors.dark` switches the variant at run time. The fonts listed
  under `font.files` are embedded into the binary. Nothing is hand-copied, so a token change
  is a rebuild.
- **iOS (SwiftUI)** — map the same names 1:1 (`accent`, `bg`, `surface`, `surface_raised`,
  `text`, `text_muted`, `danger`, `success`, `speaking_glow`, …) into a generated Swift
  `Tokens` type or asset catalog; `color.light` / `color.dark` map to the light/dark
  appearances.

## Fonts

`fonts/Inter-{Regular,SemiBold}.ttf` are Latin subsets of Inter 4.1 (SIL OFL 1.1,
`fonts/OFL.txt`) made with `pyftsubset` (U+0020–024F, general punctuation, arrows, €):
~100 KB each instead of ~410 KB. Glyphs outside the subset fall back to system fonts.

## Icons

`icons/generate.py` (Pillow) writes `icons/callxl.svg`, `icons/png/callxl-<16…1024>.png`
and `icons/callxl.ico` from one geometry definition and the dark accent colour. Outputs
are committed; rerun the script only when the icon changes.
