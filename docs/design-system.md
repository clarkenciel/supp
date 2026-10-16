# Supp design system

Supp is an offline-first list app you share with people. Its interface is quiet: stone neutrals, one blue accent and system type keep attention on the list. Bright **Memphis pops** (pink, yellow and cyan shapes with ink outlines and hard offset shadows) appear only while you are directly moving things around. When the pops show up, the app is in a mode, and leaving that mode puts everything back to calm.

The tokens live at the top of `crates/web/style.css`. Every rule below that file's token block takes its colors, spacing, radii and type sizes from them.

## Principles

1. **Quiet by default.** Use `--bg`, `--fg`, `--muted` and `--line` for almost everything. `--accent` marks the one primary action on a screen and links.
2. **Ink everywhere, pops where you act.** Every control has the Memphis *form*: 2px `--fg` outlines and `--radius-pop` corners. Pop *colour* is rationed: the one primary action per view (yellow with a pink shadow), touch moments (checked boxes, the add button, reordering) and the account badge. Content (list cards, rows, panels) stays ink on neutral, with no coloured shadows or decoration.
3. **Modes never move content.** Entering a mode must not change layout. Reorder drop targets are drawn over the existing row edges, and the e2e suite checks that rows don't shift.
4. **The platform's type and theme.** `--font-sans` is the system UI face. Light and dark follow `prefers-color-scheme`. There is no in-app toggle.

## Color

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `--bg` | `#fafaf9` | `#1c1917` | Page and sheet background. `index.html`'s `theme-color` metas copy these two values. |
| `--fg` | `#1c1917` | `#f5f5f4` | Text, and the ink outline on pop shapes. |
| `--muted` | `#78716c` | `#a8a29e` | Secondary text: hints, progress counts, done items, icon buttons. |
| `--line` | `#e7e5e4` | `#44403c` | Row dividers, input and secondary-button borders. |
| `--accent` | `#2563eb` | `#60a5fa` | Primary buttons, links, the pressed grip, focus rings. |
| `--on-accent` | `#ffffff` | `#ffffff` | Text on `--accent`. |
| `--danger` | `#dc2626` | `#f87171` | Destructive actions, offline status and errors. Outline style, never filled. |
| `--pop-pink` | `#f472b6` | same | Pop face or shadow. |
| `--pop-yellow` | `#facc15` | same | Pop face or shadow, and the fill of a pop button. |
| `--pop-cyan` | `#22d3ee` | same | Pop face or shadow, and the moving row's shadow. |
| `--pop-ink` | `#0c0a09` | same | Text on a pop fill. |

### Contrast (WCAG)

- Text: `--fg` on `--bg` is 16.7:1 (light) and 16.0:1 (dark). `--muted` is 4.6:1 and 6.9:1. `--danger` is 4.6:1 and 6.3:1. `--accent` text is 5.0:1 and 6.9:1. `--pop-ink` on `--pop-yellow` is 12.9:1.
- Primary buttons are `--pop-ink` on `--pop-yellow` (12.9:1 in both themes). This replaced white on `--accent`, which failed in dark mode (2.5:1). `--accent` now marks links and focus rings only; `--on-accent` is kept for anything that still fills with it.
- Pop colors on the light background are 1.5–2.5:1. A pop shape must therefore always carry the 1.5px `--fg` outline, which gives the shape its contrast. Never use a pop color for text or for a thin line.

## Pops

- **Rotation.** Repeated pop shapes cycle pink, then yellow, then cyan. Each shape's shadow is the next color in that cycle: a pink face has a yellow shadow, yellow has cyan, cyan has pink.
- **Outline.** A pop shape has a 1.5px stroke in `--fg` (SVG). A pop box has a 2px border or outline in `--fg`.
- **Shadow.** A hard offset with no blur: `--pop-offset-sm` (3px) on small controls, `--pop-offset-lg` (4px) on rows and cards.
- **Geometry.** Pop boxes use `--radius-pop` (4px). Pops are boxier than the rest of the UI on purpose.
- **Shape carries meaning.** Shapes say what they do, so the drop target is an arrow pointing at its gap and not an abstract blob. Color never carries the meaning on its own.

## Type

The app uses one family, `--font-sans` (`system-ui, sans-serif`), on a scale of five sizes:

| Token | Size | Where |
| --- | --- | --- |
| `--text-sm` | 0.875rem | Hints, the account bar, share links, small buttons. |
| (body) | 1rem | Everything else. |
| `--text-lg` | 1.25rem | The list title (weight 600). |
| `--text-xl` | 1.5rem | Page headings (`h1`). |
| `--text-2xl` | 1.75rem | The back chevron glyph. |

Weights: 400 for body text, 600 for titles and the item being moved, and 800 only for labels on pop fills.

## Spacing, radius and layout

- **Spacing** uses a 4px step: `--space-1` 0.25rem, `--space-2` 0.5rem, `--space-3` 0.75rem, `--space-4` 1rem, `--space-8` 2rem. Lay out siblings with `gap` and keep margins for separating sections.
- **Radius:** `--radius-pop` (4px) for every control, card and panel. `--radius-control` and `--radius-panel` remain defined but are no longer used by the app.
- **Measure:** content and the bottom sheet are capped at `--measure` (40rem) and centered.
- **Elevation:** shadows are hard offsets, never blurs, except `--shadow-sheet` under the mover sheet. The account panel takes a `--fg` offset; pops take a pink or cyan one.

## Components

- **Title bar** (`PageHeader`): the page's own controls, then the account icon, over a 2px `--fg` rule. Page and list titles are weight 800. Notices sit under it.
- **Account icon** (`AccountMenu`): an avatar ring with the person's initial and a badge for sync. Synced is a cyan cloud with an ink tick, connecting dashes the ring, offline is a red slashed badge, and anything needing attention is a yellow ! triangle. Not signed in shows a phone outline instead of the avatar. Tapping it opens a panel with the name, the sync status in words and Sign in or Sign out. Its accessible name always states the status ("Account: synced").
- **Notices**: ink-outlined boxes under the title bar for an ended session, another account's lists on this device, or a new version. Each has its fixing action.
- **Button**: the default is primary: `--pop-yellow` fill, `--pop-ink` 800-weight label, 2px `--fg` outline and a pink `--pop-offset-sm` shadow that presses in. Use at most one per view. `.secondary` is an outline with no fill; with `aria-expanded="true"` it inverts to an `--fg` fill. `.danger` is outlined in `--danger`. `.icon` is borderless and `--muted`. `.link` looks like a link. `a.button` styles a link as a button.
- **Text field**: a 2px `--line` border that turns `--fg` on focus. Inline-editable text (the list title and item text) hides its border.
- **Checkbox**: a 2px `--fg` box. Checked, it fills `--pop-cyan` with an ink tick and a 2px `--fg` offset shadow.
- **List card** (overview): a 2px `--fg` outlined card with the title, a `--muted` done count and a 4px ink progress bar on a `--line` track.
- **Item row**: checkbox, text field, grip (`⠿`) and delete (`×`), separated by `--line`. Done items get a strikethrough in `--muted`.
- **Add button** (`.fab`): a yellow hand-drawn pebble with an ink `+` and a pink shadow, fixed at the bottom centre. It is narrow and centred, so it only ever passes over item text, never a row's checkbox, grip or delete. The page footer leaves room for it, and it hides while an item is moving.
- **Add sheet**: a modal sheet from the bottom with a 2px `--fg` top rule. It stays open after each item and confirms what was added; Done, Escape or the backdrop closes it.
- **Drop arrow** (`.gutter`): the reorder target. A dotted `--fg` line at 45% runs along the row edge to a pop arrow at the right. It is zero-height, so the list never grows. Gutters aren't offered next to the moving item, because dropping there does nothing.
- **Moving row**: the picked-up row gets a 2px `--fg` outline and a `--pop-offset-lg` cyan shadow. Its delete button is hidden.
- **Mover sheet**: a fixed bottom sheet with a 2px `--fg` top border, a pink zigzag, the moving item's text, a `--muted` hint and a `.pop` Cancel button. Escape also cancels.
- **Share panel**: a 2px `--fg` outlined box with stacked content and `--muted` hints.

## Server pages

The server renders its own sign-in pages: the dev provider's form and the "Couldn't sign you in" page, for a missing invite or a disabled account. `crates/server/src/auth/page.rs` compiles `crates/web/style.css` into them, so they use the same tokens and components without needing the client built. Each page has the brand row (the pebble mark and "supp"), a title bar, a notice, then its form or a way back. With Google as the provider, the sign-in form itself is Google's page, and only the error page is ours.

## Icons and imagery

There is no icon font. Glyphs are typed characters: `‹` back, `⠿` grip, `×` delete. Use inline SVG only where a shape has to be drawn, like the drop arrow. The app icon (`crates/web/icons/`, drawn in `icon.svg`) is the add button's yellow pebble with an ink tick and a pink offset shadow, on an `fg`-dark tile. The maskable PNG shrinks the pebble into Android's safe zone, and the browser uses the SVG as its favicon.

## Voice

Labels are short and sentence case, and they say what happens: "Tap an arrow to move it there", "Clear done", "Delete list". Accessible names quote item text with curly quotes ("Move between “a” and “b”"), and the e2e tests depend on that wording.
