---
name: pg_auth
description: Postgres-enforced authorization, shown running in a terminal on a dark ink stage with one green.
colors:
  supabase-green: "#3ecf8e"
  green-bright: "#4ae29e"
  green-ink: "#0c2e1e"
  mint-code: "#d7f5e6"
  error-coral: "#f0777b"
  stage-ink: "#141414"
  pane-black: "#0f0f0f"
  strip-black: "#121212"
  tab-black: "#151515"
  step-graphite: "#1a1a1a"
  header-graphite: "#1f1f1f"
  rule-grey: "#222222"
  mark-grey: "#262626"
  frame-grey: "#2a2a2a"
  edge-grey: "#3a3a3a"
  text-muted: "#9a9a9a"
  text-secondary: "#b0b0b0"
  text-bright: "#ededed"
  text-white: "#ffffff"
typography:
  display:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "clamp(36px, 3vw, 48px)"
    fontWeight: 800
    lineHeight: 1.04
    letterSpacing: "-0.035em"
  headline:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "27px"
    fontWeight: 800
    lineHeight: 1.15
    letterSpacing: "-0.03em"
  title:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "20px"
    fontWeight: 700
    lineHeight: 1.25
    letterSpacing: "-0.02em"
  title-sm:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 700
    letterSpacing: "-0.01em"
  lede:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.55
  body:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.6
  body-sm:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.55
  label:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "11.6px"
    fontWeight: 600
    lineHeight: 1
    fontFeature: "\"liga\" 0, \"calt\" 0"
  code:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.65
    fontFeature: "\"liga\" 0, \"calt\" 0"
  button:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "14.4px"
    fontWeight: 600
    lineHeight: 1
rounded:
  chip: "5px"
  step: "7px"
  control: "8px"
  frame: "12px"
  round: "50%"
spacing:
  2xs: "6px"
  xs: "9px"
  sm: "12px"
  md: "16px"
  lg: "24px"
  xl: "48px"
  stage-gap: "52px"
  section: "72px"
components:
  button-primary:
    backgroundColor: "{colors.supabase-green}"
    textColor: "{colors.green-ink}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "12.4px 21px"
  button-primary-hover:
    backgroundColor: "{colors.green-bright}"
    textColor: "{colors.green-ink}"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.text-bright}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "12.4px 21px"
  button-quiet-hover:
    textColor: "{colors.text-white}"
  demo-frame:
    backgroundColor: "{colors.pane-black}"
    rounded: "{rounded.frame}"
  engine-switch:
    backgroundColor: "{colors.tab-black}"
    textColor: "#a8a8a8"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "2.4px"
  engine-switch-active:
    backgroundColor: "{colors.mark-grey}"
    textColor: "{colors.supabase-green}"
  step:
    backgroundColor: "{colors.step-graphite}"
    textColor: "{colors.mint-code}"
    rounded: "{rounded.step}"
    padding: "8px 10px"
  step-hover:
    backgroundColor: "#1d1d1d"
  who-chip:
    backgroundColor: "transparent"
    textColor: "#c8c8c8"
    rounded: "{rounded.chip}"
    padding: "3px 6px"
  pane-tab:
    backgroundColor: "transparent"
    textColor: "{colors.text-muted}"
    padding: "9px 14px"
  pane-tab-active:
    backgroundColor: "{colors.tab-black}"
    textColor: "{colors.text-bright}"
  act-mark:
    backgroundColor: "{colors.mark-grey}"
    textColor: "{colors.text-muted}"
    rounded: "{rounded.round}"
    size: "25px"
  act-mark-lit:
    backgroundColor: "{colors.supabase-green}"
    textColor: "{colors.green-ink}"
  file-figure:
    backgroundColor: "{colors.pane-black}"
    rounded: "{rounded.frame}"
---

# Design System: pg_auth

All sizes are given in pixels as rendered. The site is MkDocs Material, which sets the root font size to 125%, so in the source 1rem is 20px (the display size is `clamp(1.8rem, 3vw, 2.4rem)` in code). Breakpoints are written in `em` inside media queries, where 1em stays 16px.

## Overview

**Creative North Star: "The Working Terminal"**

The site is a dark console with the product running inside it. The first thing on the page is a live demo frame (psql panes for admin, anne and bob, plus the sidecar log), and everything around it is set like documentation for that console: plain Inter prose, JetBrains Mono for anything a database or a person types, thin rules between things, and one green that means "on". Nothing is decorated. Depth comes from a ladder of near-black greys and 1px frames, not from shadows or gradients.

The stage (the hero story column and the demo frame) is always dark (#141414 ground, #0f0f0f panes), in both colour schemes, because it is the brand surface and a terminal. The sections below it follow Material's scheme: in the dark scheme they stand on the same ink, in the light scheme they take Material's white with the same green. Below the stage, content is laid out as ruled lists, file-like code figures and a plain table; there are no cards with icons and no feature grid.

The density is that of a well-set README: short sections, generous vertical space between them (72px), and tight, functional spacing inside the console (6 to 12px).

**Key Characteristics:**
- Dark ink stage with a pinned, framed terminal as the hero.
- One accent, Supabase green, used for everything that is lit, pressed, done or ready.
- Inter for prose, JetBrains Mono for SQL, terminals, file names and controls inside the console.
- Depth by stepped near-blacks and 1px frames; no shadows, no gradients.
- Ruled lists and file figures below the stage, never icon cards.
- Motion is the rail fill and terminal typing; everything else is a short state change.

## Colors

A near-black graphite ladder with a single saturated green; one coral reserved for failure.

### Primary
- **Supabase Green** (`supabase-green`): the only accent. The primary button, the second line of the headline, the lit act's rail fill and marker, the pressed engine, the active pane tab's underline, a played step's check and chip, the "ready" status, the one-line file's caption and border (at 50% alpha, `#3ecf8e80`), the dots of the benchmark strip, links, focus rings and text selection (at 33% alpha, `#3ecf8e55`).
- **Lit Green** (`green-bright`): hover of the primary button and of links. Never a resting colour.
- **Deep Moss Ink** (`green-ink`): text and glyphs on green (primary button label, lit act numeral).

### Secondary
- **Mint Readout** (`mint-code`): inline code and step statements on the dark stage. A tint of the green, so code reads as "the machine's" without adding a hue.

### Tertiary
- **Failure Coral** (`error-coral`): the demo status and its dot when the boot fails. Nothing else.

### Neutral
The stage ladder, darkest to lightest:
- **Pane Black** (`pane-black`): the demo frame, terminal background, poster panes, code figures in the dark scheme.
- **Strip Black** (`strip-black`): the command strip of steps under the frame's bar.
- **Stage Ink** (`stage-ink`): the stage ground and, in the dark scheme, the whole home page and its footer.
- **Selected Tab Black** (`tab-black`): the active pane tab, the engine switcher's track, the poster's label row.
- **Step Graphite** (`step-graphite`): a step button's surface (hover lifts it to `#1d1d1d`).
- **Header Graphite** (`header-graphite`): Material's header and tabs, dark in both schemes; the dark scheme's default ground on the docs pages.
- **Rule Grey** (`rule-grey`): dividers inside the demo frame (bar, strip, tabs, boot bar, note).
- **Mark Grey** (`mark-grey`): unlit act markers, the pressed engine button, step borders, the stage's bottom edge, gutters between poster panes.
- **Frame Grey** (`frame-grey`): the demo frame's 1px border and the act rail's unlit track.
- **Edge Grey** (`edge-grey`): the quiet button's border, the who-chip's border, terminal scrollbar thumb.
- **Text White** (`text-white`): the headline and hovered or lit act titles only.
- **Text Bright** (`text-bright`): body text on the stage and the active pane tab.
- **Text Secondary** (`text-secondary`): the lede (rendered at `#b4b4b4`), boot copy, links inside the demo note.
- **Text Muted** (`text-muted`): status at rest, step notes, pane tabs at rest, unlit act numerals.

Below the stage, two scheme-following tokens replace fixed greys: the rule colour is the scheme's foreground mixed at 13% into transparent, and muted text is Material's `--md-default-fg-color--light` (#b0b0b0 in the dark scheme).

### Named Rules
**The One Green Rule.** Supabase green is the only accent and the only positive state colour: lit, pressed, done, ready, selected and focused are all green. If something new needs to look "on", it is green; if it does not mean "on", it is grey.

**The Coral Is Failure Rule.** The single non-green hue is Failure Coral, and it appears only when something failed. It is never decoration and never a second accent.

## Typography

**Display Font:** Inter (with system-ui, sans-serif)
**Body Font:** Inter (with system-ui, sans-serif)
**Label/Mono Font:** JetBrains Mono (with ui-monospace, Menlo, monospace)

**Character:** A heavy, tightly tracked Inter for headings over a calm Inter body, with JetBrains Mono doing all of the machine's talking. Ligatures are turned off in every code context so `=>` and `!=` stay as typed.

### Hierarchy
- **Display** (800, clamp(36px, 3vw, 48px), 1.04, -0.035em): the single page headline, white, its middle clause in green. Balanced wrapping.
- **Headline** (800, 27px, 1.15, -0.03em): each section heading below the stage.
- **Title** (700, 20px, 1.25, -0.02em): the three act titles, which are also buttons.
- **Title Small** (700, 16px, -0.01em): sub-headings inside a section (the "What to expect" items).
- **Lede** (400, 17px, 1.55): the one line under the headline, max 34em.
- **Body** (400, 16px, 1.6): Material's prose size; section intros, max 40em.
- **Body Small** (400, 13.6 to 14.4px, 1.55 to 1.6): act descriptions, ruled-list definitions, proof lines under a section (max 46em).
- **Label** (JetBrains Mono 600, 11.2 to 11.6px): engine switcher, pane tabs, file captions, table headers, poster pane titles.
- **Code** (JetBrains Mono 400, 12px, 1.65): code figures; the terminal runs at 12px (10px under 640px); step statements at 11.2px, 1.45.

### Named Rules
**The Machine Speaks Mono Rule.** Anything a person types or a system prints (SQL, terminal output, pane and engine names, file names, scenario names in tooltips, axis labels) is JetBrains Mono with ligatures off. Anything that explains is Inter.

**The One Headline Rule.** Display size appears once, on the stage. Section headings stop at 27px.

## Layout

A centred wrap of max 1520px (76rem) with a 24px side gutter.

The stage is a two-column grid: the story column at up to 520px and the demo frame taking the rest, 52px apart, top-aligned. The demo frame is sticky (68px from the top, under Material's header), its height `min(840px, 100vh - 88px)` with a 600px floor, so it stays in view while the acts are read. The stage pads 44px above and 60px below and ends with a 1px Mark Grey rule.

Inside the frame the order is fixed: bar (engine switcher left, status right), command strip of the lit act's steps (an auto-fit grid of columns at least 300px wide, 6px gaps), pane tabs, the screen, then the boot bar and a one-line note. Before boot the screen shows a recording of Act I in a 2x2 grid of panes split by 1px gutters (admin and sidecar log spanning both columns), and the boot bar sits under it, never over it.

Sections below the stage are spaced 72px apart. Inside them: code comparisons on a 1.25 : 1 grid with 16px gaps; ruled lists in two columns 48px apart; the expectation items in three columns (the first spanning all three, split again 0.9 : 1.1 around the strip plot); the install block on a 0.8 : 1.2 grid with a 48px gap.

Responsive behaviour:
- **Under 960px (60em):** everything goes to one column. The demo frame stops being sticky and sits under the acts; its screen is 480px tall once running, and before boot the recording sets its height with its panes stacked.
- **Under 640px (40em):** the pane tabs drop their keyboard hints and share the width equally; the boot bar wraps; the terminal drops to 10px.

## Elevation & Depth

Flat. Depth is tonal: the stage ladder steps from Pane Black up through Strip Black, Stage Ink, Tab Black and Step Graphite, and every boundary is a 1px line (Rule Grey inside the frame, Frame Grey around it, the 13% scheme rule below the stage). There are no drop shadows and no gradients anywhere.

Two `box-shadow` uses exist and both are rules, not elevation: the active pane tab's 2px inset green underline (`inset 0 -2px 0 #3ecf8e`), and a 2px ring in the page ground around each strip-plot dot so overlapping dots stay separate.

### Named Rules
**The Frame Not Float Rule.** A surface is separated by a 1px line and a step on the grey ladder, never by a shadow. If two surfaces need more separation, move one a step on the ladder.

## Shapes

Gently rounded rectangles, with radius growing with the size of the thing: 5px on the who-chip, 7px on step buttons, 8px on buttons and the engine switcher's track (its inner buttons at about 5.6px), 12px on the demo frame and the code figures. Full circles for the act markers, the status dot and the strip-plot dots. The act rail is a 2px vertical line; the "same time" mark on the strip plot is a 1px dashed line. Borders are always 1px.

## Components

### Buttons
Solid and direct; one green, one outline.
- **Shape:** gently rounded (8px).
- **Primary:** Supabase Green fill and border, Deep Moss Ink label, Inter 600 at 14.4px, padding 12.4px by 21px, with a 16px inline play glyph where it boots the demo. Used for "Boot the live demo" (twice: in the story column and in the boot bar) and "Getting started".
- **Quiet:** transparent with a 1px Edge Grey border and Text Bright label; hover turns the border green and the label white. Sits beside the primary, never alone.
- **Hover / Focus:** 160ms colour change only; primary hover is Lit Green. Focus is a 2px green outline offset 2px across the stage and sections.

### Engine Switcher
A segmented control in the frame's bar: a Tab Black track with a 1px `#2e2e2e` border and 2.4px inner padding, holding OpenFGA, SpiceDB and Cerbos as mono labels. The pressed engine (`aria-pressed`) gets a Mark Grey fill and green label; the others are `#a8a8a8`, white on hover. Picking Cerbos swaps every relationship step and act description for its attribute version.

### Command Strip Steps (signature)
The lit act's steps, as buttons in the strip above the panes. Each is a three-area grid: a who-chip (the pane it types into: admin, anne, bob) on the left, the SQL statement in Mint Readout mono with a muted note under it, and a check slot on the right. Step Graphite surface, 1px Mark Grey border, 7px radius; hover lifts the surface and tints the border green at 50%. Once played, the check fades in green and the who-chip turns green. When an act's last step is played, the next act lights and its steps replace these.

### Pane Tabs
A row of mono tabs (admin, anne, bob, sidecar log) separated by 1px Rule Grey lines, each with a dim `alt N` hint. At rest Text Muted; active is Text Bright on Tab Black with the 2px inset green underline. Clicking a step switches to its pane.

### Act Rail (signature)
The three acts as an ordered list on a 2px rail. Each act has a 25px circular marker with a roman numeral in mono; unlit markers are Mark Grey with muted numerals, the lit act and every act before it are green with Deep Moss Ink numerals. A green fill runs down the rail to the lit act's marker (700ms, `cubic-bezier(0.16, 1, 0.3, 1)`); the markers change colour 200ms after it starts. Act titles are buttons in Title type, `#cfcfcf` at rest and white when lit or hovered.

### Demo Status
Mono text at the bar's right edge with a 8px dot before it: grey at rest ("not started"), green and pulsing while loading, solid green when running, Failure Coral on error.

### File Figures (Cards / Containers)
Code shown as files rather than cards.
- **Corner Style:** 12px.
- **Background:** the code ground (Pane Black in the dark scheme, Material's code background in light).
- **Shadow Strategy:** none (see Elevation & Depth).
- **Border:** 1px scheme rule; the figure that carries the one-line answer gets a 50% green border and a green caption note.
- **Internal Padding:** caption row 10px by 16px in mono 600, with a muted note right-aligned; code 16px by 18px, 42px on the right to clear Material's copy button. Comments in the code are muted; the key relations are bold green.

### Ruled Lists and Table
Below the stage, items are separated by a 1px top rule with 16 to 20px vertical padding: the "mistakes you cannot make" definition list (bold term, muted definition), the expectation items, and the engine table (mono muted header, bold engine names, no row hover fill).

### Benchmark Strip
One green dot per scenario on a log axis from 0.2x to 2x, stacked in three rows, a dashed "same time" line at 1x, mono axis labels. Each dot has an 18px hit target and on hover or focus shows a mono tooltip in inverted scheme colours with a 6px radius.

## Do's and Don'ts

### Do:
- **Do** keep the stage on Stage Ink (#141414) with Pane Black (#0f0f0f) panes in both colour schemes.
- **Do** use Supabase Green (#3ecf8e) for every lit, pressed, done, ready and focused state, and Deep Moss Ink (#0c2e1e) for anything written on it.
- **Do** separate surfaces with 1px lines (Rule Grey #222 inside the frame, Frame Grey #2a2a2a around it, the 13% scheme rule below the stage) and steps on the grey ladder.
- **Do** set SQL, terminal output, pane names, engine names and file captions in JetBrains Mono with ligatures off.
- **Do** present code as a file: a caption row naming the file and a muted note, then the code, in a 12px-radius figure.
- **Do** lay out supporting points as ruled lists or a plain table under a 27px headline.
- **Do** limit authored motion to the rail fill (700ms, `cubic-bezier(0.16, 1, 0.3, 1)`) and terminal typing; other state changes are 160 to 300ms colour or opacity transitions, and all of it stops under `prefers-reduced-motion`.

### Don't:
- **Don't** use gradients or drop shadows for depth.
- **Don't** add a second accent hue; Failure Coral (#f0777b) is for failure only.
- **Don't** put features in a grid of cards or icon tiles; use ruled lists, file figures or a table.
- **Don't** split the hero into a headline over a feature grid; the stage is the story column beside the working demo frame.
- **Don't** lay anything over the demo recording or terminal; controls go in the bar, the strip, the tabs or the boot bar around it.
- **Don't** make scrolling type or run anything; a statement runs only when someone clicks a step or types.
