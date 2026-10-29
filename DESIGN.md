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
  title-sm:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 700
    letterSpacing: "-0.01em"
  step-title:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "13.2px"
    fontWeight: 700
    lineHeight: 1.2
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
    fontSize: "13.6px"
    fontWeight: 400
    lineHeight: 1.55
  caption:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "12.4px"
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
  inner: "5.6px"
  control: "8px"
  frame: "12px"
  round: "50%"
spacing:
  2xs: "6px"
  xs: "9px"
  sm: "12px"
  md: "16px"
  lg: "24px"
  col: "40px"
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
    rounded: "{rounded.inner}"
  step:
    backgroundColor: "{colors.step-graphite}"
    textColor: "{colors.text-white}"
    typography: "{typography.step-title}"
    rounded: "{rounded.control}"
    padding: "10px 11px"
  step-next:
    backgroundColor: "#1c211e"
  step-mark:
    backgroundColor: "{colors.frame-grey}"
    textColor: "#cfcfcf"
    rounded: "{rounded.round}"
    size: "22px"
  step-mark-lit:
    backgroundColor: "{colors.supabase-green}"
    textColor: "{colors.green-ink}"
  protect-line:
    backgroundColor: "{colors.pane-black}"
    textColor: "{colors.mint-code}"
    typography: "{typography.code}"
    rounded: "{rounded.control}"
    padding: "12px 15px"
  file-figure:
    backgroundColor: "{colors.pane-black}"
    typography: "{typography.code}"
    rounded: "{rounded.frame}"
    padding: "16px 42px 16px 18px"
  why-table:
    rounded: "{rounded.frame}"
    padding: "9px 14px"
---

# Design System: pg_auth

All sizes are given in pixels as rendered. The site is MkDocs Material, which sets the root font size to 125%, so in the source 1rem is 20px (the display size is `clamp(1.8rem, 3vw, 2.4rem)` in code). Breakpoints are written in `em` inside media queries, where 1em stays 16px.

## Overview

**Creative North Star: "The Working Terminal"**

The site is a dark console with the product running inside it. The first thing on the page is a live demo frame (two psql panes, one as anne and one as bob, with three numbered steps under them), and everything around it is set like documentation for that console: plain Inter prose, JetBrains Mono for anything a database or a person types, thin rules between things, and one green that means "on". Nothing is decorated. Depth comes from a ladder of near-black greys and 1px frames, not from shadows or gradients.

The stage (the hero story column and the demo frame) is always dark (#141414 ground, #0f0f0f panes), in both colour schemes, because it is the brand surface and a terminal. The sections below it follow Material's scheme: in the dark scheme they stand on the same ink, in the light scheme they take Material's white with the same green. Below the stage, content is laid out as ruled lists, file-like code figures and plain tables; there are no cards with icons and no feature grid.

The density is that of a well-set README: short sections, generous vertical space between them (72px), and tight, functional spacing inside the console (6 to 12px).

**Key Characteristics:**
- Dark ink stage with a framed terminal beside the story as the hero.
- One accent, Supabase green, used for everything that is lit, pressed, done or ready.
- Inter for prose, JetBrains Mono for SQL, terminals, pane names and controls inside the console.
- Depth by stepped near-blacks and 1px frames; no shadows, no gradients.
- Ruled lists, code figures and plain tables below the stage, never icon cards.
- Motion is terminal typing and the lit step moving on; everything else is a short state change.

## Colors

A near-black graphite ladder with a single saturated green; one coral reserved for failure.

### Primary
- **Supabase Green** (`supabase-green`): the only accent. The primary button, the second line of the headline, the lit step's border and numeral, every played step's numeral and check, the pressed engine, the active tmux pane's title, the "ready" status, the "yes" column of the authz.why table, the bold relation names in code figures, the numerals of the two-step list, the dots of the benchmark strip, links, focus rings and text selection (at 33% alpha, `#3ecf8e55`). A step's hover border is the green at 50% alpha (`#3ecf8e80`).
- **Lit Green** (`green-bright`): hover of the primary button and of links. Never a resting colour.
- **Deep Moss Ink** (`green-ink`): text and glyphs on green (primary button label, lit step numerals, two-step list numerals).

### Secondary
- **Mint Readout** (`mint-code`): inline code, the protect line and step statements on the dark stage. A tint of the green, so code reads as "the machine's" without adding a hue.

### Tertiary
- **Failure Coral** (`error-coral`): the demo status and its dot when the boot fails, and the "no" cells of the authz.why rows that failed. Nothing else.

### Neutral
The stage ladder, darkest to lightest:
- **Pane Black** (`pane-black`): the demo frame, the terminal background, the recording's panes, the protect line, code figures in the dark scheme.
- **Strip Black** (`strip-black`): the row of three steps under the panes.
- **Stage Ink** (`stage-ink`): the stage ground and, in the dark scheme, the whole home page and its footer.
- **Selected Tab Black** (`tab-black`): the engine switcher's track and the recording's label row.
- **Step Graphite** (`step-graphite`): a step's surface. The lit step takes a green-tinted graphite (`#1c211e`) instead.
- **Header Graphite** (`header-graphite`): Material's header and tabs, dark in both schemes; the dark scheme's default ground on the docs pages.
- **Rule Grey** (`rule-grey`): dividers inside the demo frame (bar, steps row, boot bar, foot).
- **Mark Grey** (`mark-grey`): the pressed engine button, step borders, the stage's bottom edge, the gutter between the recording's panes.
- **Frame Grey** (`frame-grey`): the demo frame's and the protect line's 1px border, and the disc of a step numeral not yet lit.
- **Edge Grey** (`edge-grey`): the quiet button's border, the tmux pane borders, the terminal scrollbar thumb.
- **Text White** (`text-white`): the headline, step titles and the hovered quiet button only.
- **Text Bright** (`text-bright`): body text on the stage, the names in the demo title, the suggestion that follows the third step.
- **Text Secondary** (`text-secondary`): the lede (rendered at `#b4b4b4`), boot copy, links in the frame's foot.
- **Text Muted** (`text-muted`): the demo title and status at rest, step notes. Quieter copy on the stage (the setup lines, the where-it-runs line, the recording's label) sits at `#a3a3a3`; the foot's own copy, the engine row's lead label and the recording's pane names at `#8f8f8f`.

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
- **Title Small** (700, 16px, -0.01em): sub-headings inside a section (the two sides of the comparison, the "What to expect" items).
- **Step Title** (700, 13.2px): the plain-words name of each demo step ("Who sees what"), white.
- **Lede** (400, 17px, 1.55): the paragraph under the headline, max 34em.
- **Body** (400, 16px, 1.6): Material's prose size; section intros, max 40em.
- **Body Small** (400, 13.2 to 14.4px, 1.55 to 1.6): the setup lines in the story column, comparison notes, ruled-list definitions, the authz.why table, proof lines under a section (max 46em).
- **Caption** (400, 12.4px, 1.55): the where-it-runs line under the actions, the benchmark strip's caption. Inside the demo frame, Inter fine print drops to 10.8 to 11.6px (step notes, boot copy, the foot).
- **Label** (JetBrains Mono 400 to 600, 10.8 to 11.6px): the demo title and status, engine switcher, the recording's label and pane names, table headers.
- **Code** (JetBrains Mono 400, 12px, 1.65): code figures and the protect line (1.6). The terminal runs at 11px; the recording at 11.6px, 1.5; step statements at 10.8px, 1.45.

### Named Rules
**The Machine Speaks Mono Rule.** Anything a person types or a system prints (SQL, terminal output, pane and engine names, file names, scenario names in tooltips, axis labels) is JetBrains Mono with ligatures off. Anything that explains is Inter.

**The One Headline Rule.** Display size appears once, on the stage. Section headings stop at 27px.

## Layout

A centred wrap of max 1520px (76rem) with a 24px side gutter.

The stage is a two-column grid: the story column at up to 520px and the demo frame taking the rest, 52px apart, top-aligned. The story column reads top to bottom: headline, lede, the setup (which engine holds the rules, the one protect line, the rules in plain words), the actions, then the where-it-runs line. The demo frame is sticky (68px from the top, under Material's header), its height `min(840px, 100vh - 88px)` with a 600px floor. The stage pads 44px above and 60px below and ends with a 1px Mark Grey rule.

Inside the frame the order is fixed: the bar (the demo title left, the status right), the screen, the row of three steps (equal columns, 7px apart, on Strip Black), the boot bar, then the foot (the engine switcher left, a quiet hint right). Before boot the screen shows step 1 as recorded: a label row across the top, then anne's and bob's panes side by side, split by a 1px gutter; the boot bar sits under the steps, never over the recording. Once running, the screen is the terminal (tmux with two titled panes, the active pane's title in green), the boot bar and the hero's boot button go, and after the third step a suggestion to try by hand opens the foot at full width.

Sections below the stage are spaced 72px apart. Inside them: the comparison on two equal columns 40px apart; ruled lists in two columns 48px apart; the expectation items in two columns 40px apart (the first spanning both, split again 0.9 : 1.1 around the strip plot, 48px apart); the authz.why table at full width; the install block on a 0.8 : 1.2 grid with a 48px gap.

Responsive behaviour:
- **Under 960px (60em):** everything goes to one column. The demo frame stops being sticky and sits under the story; its screen is 480px tall once running, and before boot the recording sets its height with its panes stacked. Pressing the hero's boot button brings the frame into view.
- **Under 640px (40em):** the three steps stack; the boot bar wraps; code figures and the protect line wrap their lines instead of scrolling; the terminal's tmux panes stack too.

## Elevation & Depth

Flat. Depth is tonal: the stage ladder steps from Pane Black up through Strip Black, Stage Ink, Tab Black and Step Graphite, and every boundary is a 1px line (Rule Grey inside the frame, Frame Grey around it, the 13% scheme rule below the stage). There are no drop shadows and no gradients anywhere.

One `box-shadow` exists and it is a rule, not elevation: a 2px ring in the page ground around each strip-plot dot so overlapping dots stay separate.

### Named Rules
**The Frame Not Float Rule.** A surface is separated by a 1px line and a step on the grey ladder, never by a shadow. If two surfaces need more separation, move one a step on the ladder.

## Shapes

Gently rounded rectangles, with radius growing with the size of the thing: about 5.6px on the engine switcher's inner buttons, 6px on the strip-plot tooltip, 8px on buttons, steps, the protect line and the engine switcher's track, 12px on the demo frame, code figures and the authz.why table. Full circles for the step numerals, the two-step list's numerals, the status dot and the strip-plot dots. The "same time" mark on the strip plot is a 1px dashed line. Borders are always 1px.

## Components

### Buttons
Solid and direct; one green, one outline.
- **Shape:** gently rounded (8px).
- **Primary:** Supabase Green fill and border, Deep Moss Ink label, Inter 600 at 14.4px, padding 12.4px by 21px, with a 16px inline play glyph where it boots the demo. Used for "Boot the live demo" (twice before boot: in the story column and in the boot bar; both go once the demo runs) and "Getting started".
- **Quiet:** transparent with a 1px Edge Grey border and Text Bright label; hover turns the border green and the label white. It sits beside the primary, and stays on alone once the demo runs.
- **Hover / Focus:** 160ms colour change only; primary hover is Lit Green. Focus is a 2px green outline offset 2px across the stage and sections.

### Steps (signature)
The three things to do, numbered, in a row under the panes. Each step is a button: a 22px numeral disc on the left, then the Step Title, the SQL statement in Mint Readout mono and a muted note under it, and a check slot on the right. Step Graphite surface, 1px Mark Grey border, 8px radius, padding 10px by 11px; hover tints the border green at 50%. The next step to play is lit: green border, a green-tinted graphite surface (`#1c211e`) and a green numeral with a Deep Moss Ink figure. Clicking types the statement into its pane (step 1 into both), the check fades in green, the numeral stays green and the light moves to the next step. Unlit numerals are Frame Grey discs with `#cfcfcf` figures. Picking another engine clears every check and lights step 1 again; Cerbos swaps step 2 for its attribute version.

### Engine Switcher
A segmented control in the frame's foot: a Tab Black track with a 1px `#2e2e2e` border and 2.4px inner padding, led by a muted mono label ("Run the steps on"), then OpenFGA, SpiceDB and Cerbos as mono labels. The pressed engine (`aria-pressed`) gets a Mark Grey fill and green label; the others are `#a8a8a8`, white on hover. Switching engines reconnects both panes and resets that engine's data.

### Demo Status
Mono text at the bar's right edge with an 8px dot before it: grey at rest ("not started"), green and pulsing while loading, solid green when running, Failure Coral on error. The bar's left edge carries the demo title in muted mono with the two user names in Text Bright.

### Protect Line
The one call that protects the table, shown in the story column: Pane Black ground, 1px Frame Grey border, 8px radius, padding 12px by 15px, code in Mint Readout mono at 12px. A setup line above it names the engine; a line below states the rules in plain words.

### File Figures (Cards / Containers)
Code shown as a framed file rather than a card.
- **Corner Style:** 12px.
- **Background:** the code ground (Pane Black in the dark scheme, Material's code background in light).
- **Shadow Strategy:** none (see Elevation & Depth).
- **Border:** 1px scheme rule.
- **Internal Padding:** 16px by 18px, 42px on the right to clear Material's copy button. Comments in the code are muted; the key relations are bold green. Where two figures make a sequence, they sit in a numbered list whose numerals are 22px green discs with Deep Moss Ink figures.

### Ruled Lists and Tables
Below the stage, items are separated by a 1px top rule with 16 to 20px vertical padding: the "mistakes you cannot make" definition list (bold term, muted definition), the expectation items, and the engine table (mono muted header, bold engine names, no row hover fill). The authz.why table is the one framed table: a 1px scheme rule around it at 12px radius, a mono muted header, the step name in mono, the "ok" column in green mono, and the rows that failed set in the foreground at 600 with their "no" in Failure Coral.

### Benchmark Strip
One green dot per scenario on a log axis from 0.2x to 2x, stacked in three rows, a dashed "same time" line at 1x, mono axis labels. Each dot has an 18px hit target and on hover or focus shows a mono tooltip in inverted scheme colours with a 6px radius.

## Do's and Don'ts

### Do:
- **Do** keep the stage on Stage Ink (#141414) with Pane Black (#0f0f0f) panes in both colour schemes.
- **Do** use Supabase Green (#3ecf8e) for every lit, pressed, done, ready and focused state, and Deep Moss Ink (#0c2e1e) for anything written on it.
- **Do** separate surfaces with 1px lines (Rule Grey #222 inside the frame, Frame Grey #2a2a2a around it, the 13% scheme rule below the stage) and steps on the grey ladder.
- **Do** set SQL, terminal output, pane names, engine names and table headers in JetBrains Mono with ligatures off.
- **Do** present code as a framed file: a 12px-radius figure with a 1px rule, comments muted and the key relation in bold green.
- **Do** lay out supporting points as ruled lists or a plain table under a 27px headline.
- **Do** limit authored motion to terminal typing and the lit step moving to the next one (a 200ms border and surface change); other state changes are 160 to 300ms colour or opacity transitions, and all of it stops under `prefers-reduced-motion`.

### Don't:
- **Don't** use gradients or drop shadows for depth.
- **Don't** add a second accent hue; Failure Coral (#f0777b) is for failure only.
- **Don't** put features in a grid of cards or icon tiles; use ruled lists, file figures or a table.
- **Don't** split the hero into a headline over a feature grid; the stage is the story column beside the working demo frame.
- **Don't** lay anything over the demo recording or terminal; controls go in the bar, the steps row, the boot bar or the foot around it.
- **Don't** make scrolling type or run anything; a statement runs only when someone clicks a step or types.
