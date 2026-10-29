// The live demo on the home page: Postgres with pg_auth, three engines and their sidecars, booted
// in this tab.
//
// The machine is a snowglobe build of docker/demo (`mise run demo` writes it to docs/demo/): an
// arm64 Linux snapshot restored by armless, a CPU emulator compiled to WebAssembly. This file uses
// only the build's assets (snapshot, filesystem blobs, armless and xterm) and boots the machine
// straight into the page, so the terminal is ours to style and to type into.
//
// Inside, tmux holds two psql panes, anne and bob; alt-1 and alt-2 switch between them. Each engine
// has its own database; switching engines reconnects both panes to it.
(() => {
  const root = document.querySelector("[data-demo]");
  if (!root) return;

  const base = new URL(root.dataset.demo, document.baseURI);
  const at = (path) => new URL(path, base).href;
  const screen = root.querySelector(".pa-demo__screen");
  const boots = [...root.querySelectorAll("[data-boot]")];
  const status = root.querySelector(".pa-demo__status");
  const engines = [...root.querySelectorAll("[data-engine]")];
  const steps = [...root.querySelectorAll(".pa-step")];
  const views = [...root.querySelectorAll("[data-view]")];
  const PANE = { anne: "\x1b1", bob: "\x1b2" };
  const NAME = { openfga: "OpenFGA", spicedb: "SpiceDB", cerbos: "Cerbos" };
  // a phone: the panes stacked (tmux alt-v) so a line is not wrapped mid-word in half the width
  const narrow = matchMedia("(max-width: 40em)");
  // openfga and spicedb run the same relationship steps; cerbos decides on fields
  const FAMILY = { openfga: "relationships", spicedb: "relationships", cerbos: "attributes" };
  let emulator = null;
  let started = false;

  const setStatus = (state, text) => {
    root.dataset.state = state;
    status.textContent = text;
  };

  const script = (src) =>
    new Promise((resolve, reject) => {
      const el = document.createElement("script");
      el.src = src;
      el.onload = resolve;
      el.onerror = () => reject(new Error(`could not load ${src}`));
      document.head.append(el);
    });

  const stylesheet = (href) => {
    const el = document.createElement("link");
    el.rel = "stylesheet";
    el.href = href;
    document.head.append(el);
  };

  async function start() {
    if (started) return;
    started = true;
    setStatus("loading", "loading the machine");

    try {
      const run = await fetch(at("run.json")).then((r) => {
        if (!r.ok) throw new Error("demo not built");
        return r.json();
      });
      stylesheet(at("xterm/xterm.css"));
      await Promise.all([script(at("xterm/xterm.js")), script(at("armless/armless.js"))]);
      await script(at("xterm/addon-fit.js"));

      // Everything here must match the snapshot: same RAM and CPU count. One CPU, so no
      // cross-origin isolation is needed and the docs page keeps its ordinary headers.
      emulator = new Armless({
        wasm: at("armless/armless.wasm"),
        wasm_smp: at("armless/armless-smp.wasm"),
        memory_mb: run.memory_mb,
        cpus: run.cpus,
        filesystem: { json: at("system/filesystem.json"), baseurl: at("system/filesystem/") },
        initial_state: at("system/state.bin.zst"),
        autostart: false,
      });

      emulator.on("progress", (e) => {
        if (e.file.endsWith("state.bin.zst") && e.total) {
          setStatus("loading", `restoring ${Math.round((e.loaded / e.total) * 100)}%`);
        }
      });
      emulator.on("error", (error) => setStatus("error", `failed: ${error.message || error}`));

      const replay = fetch(at("system/console.bin"))
        .then((r) => (r.ok ? r.arrayBuffer() : null))
        .catch(() => null);

      await emulator.ready;

      const term = new Terminal({
        cursorBlink: true,
        scrollback: 2000,
        fontFamily: '"JetBrains Mono", ui-monospace, Menlo, monospace',
        fontSize: 11,
        theme: {
          background: "#0f0f0f",
          foreground: "#e6e6e6",
          cursor: "#3ecf8e",
          selectionBackground: "#3ecf8e55",
          green: "#3ecf8e",
          brightGreen: "#4ae29e",
        },
      });
      const fit = new FitAddon.FitAddon();
      term.loadAddon(fit);
      screen.replaceChildren();
      term.open(screen);
      term.textarea?.setAttribute("autocapitalize", "off");
      term.textarea?.setAttribute("spellcheck", "false");

      // Draw what tmux last drew at the snapshot's size, then fit and let tmux redraw
      term.resize(run.console_cols || 90, run.console_rows || 30);
      const bytes = await replay;
      if (bytes) await new Promise((resolve) => term.write(new Uint8Array(bytes), resolve));

      emulator.on("console", (out) => term.write(out));
      term.onData((data) => emulator.console_input(data));

      const resize = () => {
        fit.fit();
        emulator.console_resize(term.cols, term.rows);
      };
      new ResizeObserver(resize).observe(screen);
      resize();
      const layout = () => emulator.console_input(narrow.matches ? "\x1bv" : "\x1bh");
      narrow.addEventListener("change", layout);

      // the wheel scrolls the page, not tmux: the demo fills half the first screen, where the
      // pointer rests
      screen.addEventListener("wheel", (e) => e.stopPropagation(), { capture: true });

      emulator.run();
      root.classList.add("is-running");
      setStatus("ready", "running in this tab");
      if (narrow.matches) layout();
      // the snapshot is connected to openfga: follow an engine picked before booting
      if (root.dataset.engine !== "openfga") connect(root.dataset.engine);
      // the terminal takes the keyboard only when clicked: a step or the boot button leaves focus
      // where it was, so Tab still moves through the page (and a phone opens no keyboard)
      preloadWarmPack().catch(() => {});
    } catch (error) {
      started = false;
      setStatus("error", error.message === "demo not built" ? "demo not built (mise run demo)" : `failed: ${error.message}`);
    }
  }

  // The files the first commands read, in one background download instead of one request each
  async function preloadWarmPack() {
    const response = await fetch(at("system/warm.pack"));
    if (!response.ok) return;
    const pack = new Uint8Array(await response.arrayBuffer());
    const indexSize = new DataView(pack.buffer).getUint32(0, true);
    const index = JSON.parse(new TextDecoder().decode(pack.subarray(4, 4 + indexSize)));
    let offset = 4 + indexSize;
    for (const [name, , length] of index) {
      const blob = pack.subarray(offset, offset + length);
      offset += length;
      if (!emulator.has_blob(name)) emulator.provide_blob(name, blob);
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }

  const ready = () => emulator && root.dataset.state === "ready";

  // Switch pane, clear whatever is half-typed there (ctrl-u), then type the line
  const type = (pane, line) => emulator.console_input(`${PANE[pane]}\x15${line}\r`);

  // The steps shown for this engine, in order; the first not yet played is the next one
  const shown = () => steps.filter((s) => s.offsetParent);
  const markNext = () => {
    const next = shown().find((s) => !s.classList.contains("is-done"));
    steps.forEach((s) => s.classList.toggle("is-next", s === next));
    // all played: the foot suggests one to try by hand
    root.classList.toggle("is-played", !next);
  };

  // Reconnect both panes to the engine's database, put its data back as the demo starts (a second
  // visit tells the same story), and clear them (ctrl-l)
  function connect(engine) {
    if (!ready()) return;
    type("bob", `\\c ${engine}`);
    type("anne", `\\c ${engine}`);
    type("anne", "select demo_reset();");
    for (const pane of ["bob", "anne"]) emulator.console_input(`${PANE[pane]}\x0c`);
  }

  function pickEngine(engine) {
    if (engine === root.dataset.engine) return;
    root.dataset.engine = engine;
    root.dataset.family = FAMILY[engine];
    root.querySelectorAll("[data-engine-name]").forEach((n) => (n.textContent = NAME[engine]));
    engines.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.engine === engine)));
    // another database: its steps start over
    steps.forEach((s) => s.classList.remove("is-done"));
    markNext();
    connect(engine);
  }

  // Demo or Setup: the terminal, or what the demo's database ran and the engine's own rules
  function showView(view) {
    root.dataset.view = view;
    views.forEach((v) => v.setAttribute("aria-selected", String(v.dataset.view === view)));
  }

  async function send(step) {
    showView("demo");
    if (!started) await start();
    if (!ready()) return;
    const panes = step.dataset.pane === "both" ? ["anne", "bob"] : [step.dataset.pane];
    for (const pane of panes) type(pane, step.dataset.cmd);
    step.classList.add("is-done");
    markNext();
  }

  // the hero's boot button is above the demo on a phone: bring the demo into view as it boots
  boots.forEach((b) =>
    b.addEventListener("click", () => {
      start();
      if (!b.closest(".pa-demo") && matchMedia("(max-width: 60em)").matches) {
        const smooth = !matchMedia("(prefers-reduced-motion: reduce)").matches;
        root.querySelector(".pa-demo").scrollIntoView({ block: "start", behavior: smooth ? "smooth" : "auto" });
      }
    }),
  );
  engines.forEach((b) => b.addEventListener("click", () => pickEngine(b.dataset.engine)));
  steps.forEach((s) => s.addEventListener("click", () => send(s)));
  views.forEach((v) => v.addEventListener("click", () => showView(v.dataset.view)));
})();
