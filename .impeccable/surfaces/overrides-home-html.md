---
version: 1
slug: "overrides-home-html"
primary_target: "overrides/home.html"
related_targets: ["docs/stylesheets/home.css"]
---

# Landing page (overrides/home.html)

Scope: the site's home page. Mode: Persuade. Audience: app builders on Postgres with no backend
(Supabase/PostgREST from the browser). Action: boot the live demo and play with it. Proof: the demo
itself (Postgres, pg_auth, sidecar, OpenFGA/SpiceDB/Cerbos in the tab), real SQL, the measured
grant latency, parity and coverage. Constraints: experimental status stated plainly; benchmarks
only set expectations; no internals on this page; no benchmark brag, no generic SaaS grid.

## Direction contract

Revised 2026-10-29 after user feedback ("the demos are very complicated") and three persona tests
(a Supabase builder, an engineer running OpenFGA, a newer developer on a phone): two users and three
clicks replaced the three acts, four panes and ten steps.

THESIS: two users of one app, Anne and Bob, and three clicks: the same query shows them different
rows, Anne writes one row, Bob sees more. Beside it, in plain words, what runs where. Refuses the
split hero over a feature-card grid, and refuses teaching every feature in the demo.

OWN-WORLD: pg_auth's dark ink ground (#141414, #0f0f0f panes), Supabase green #3ecf8e as the only
accent and state colour (a coral only for failure), Inter for prose, JetBrains Mono for SQL,
terminals and labels; rules and 1px #2a2a2a frames, no gradients, no icon tiles.

STORY: the visitor understands that their app can talk to Postgres directly and still get real
authorization, because an engine holds the rules and Postgres enforces them; believes it because
they watch Bob's empty result fill after Anne inserts one row, and Bob's own write change 0 rows;
learns at once where it runs (Postgres they run themselves; not hosted Supabase yet) and that it is
experimental; does: boot the demo, click the three steps, try the suggested query.

FIRST VIEWPORT: left 520px column: headline "Your app's permissions, enforced by Postgres. No
backend.", a lede saying what "no backend" means, the engine and the one protect line with the rules
in plain words, "Boot the live demo" and a quiet guide link, then the where-it-runs line; right: the
demo frame pinned (title, status), two psql panes (anne, bob; stacked on a phone) showing step 1 as
recorded before boot, the three numbered steps under them, the boot bar, and a quiet engine row.

FORM: chosen sketch, position 2 of the dealt three (a sticky demo beside the story),
seed:b0da61fe066f10c1, simplified as above. Signature interaction: each step types its statement
into its pane (step 1 into both); the next step is lit; after step 3 a hint offers a query to try by
hand. Switching engines reconnects both panes and resets that database's data. Scrolling types
nothing. Motion grammar: terminal text types; the lit step changes; nothing else moves. Below the
stage: one rule written both ways, authz.why as a table, the mistakes RLS invites, what to expect
(cost as a strip of dots, freshness, the engine down, where it runs, status), engines, install.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.
