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

THESIS: one app (a team document editor) followed from model to grant to "why can't I see it?",
told in three acts beside a live demo that follows the act being read; refuses the split hero over a
feature-card grid.

OWN-WORLD: pg_auth's dark ink ground (#141414, #0f0f0f panes), Supabase green #3ecf8e as the only
accent and state colour, Inter for prose, JetBrains Mono for SQL, terminals and labels; rules and
1px #2a2a2a frames, no gradients, no icon tiles.

STORY: the visitor understands that rules live in one model and Postgres enforces them for every
client; believes it because they watch a row they insert give Bob access and authz.why explain a
hidden row; does: boot the demo and click through the acts, then read the no-backend guide.

FIRST VIEWPORT: left 520px column: headline "Your app's permissions, enforced by Postgres. No
backend.", one-line lede, the three acts as a vertical timeline (Act I active), primary action
"Boot the live demo" and a quiet guide link; right: the demo frame (window bar with status, tabs
admin/anne/bob/sidecar, Act I's SQL typed, its output below) pinned while the acts scroll.

FORM: chosen sketch, position 2 of the dealt three (structure 2 on my ordered list: a sticky demo
beside a three-act story); seed:b0da61fe066f10c1. Signature interaction: the lit act's steps sit in the
pinned frame as a command strip, and clicking one types it into its pane; an act's last step lights
the next act. Scrolling types nothing: a statement runs only when the visitor asks for it. Engine
switcher (OpenFGA, SpiceDB, Cerbos) in the frame. Before boot the frame shows Act I as recorded
from the demo. Motion grammar: the green fill travels the timeline rail to the lit act; terminal
text types; nothing else moves.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.
