// The live demo on the home page: Postgres with pg_auth, three engines and their sidecars, booted
// in this tab.
//
// The machine is a snowglobe build of docker/demo (`mise run demo` writes it to docs/demo/): an
// arm64 Linux snapshot restored by armless, a CPU emulator compiled to WebAssembly. This file uses
// only the build's assets (snapshot, filesystem blobs, armless and xterm) and boots the machine
// straight into the page, so the terminal is ours to style and to type into.
//
// Inside, tmux holds four panes: admin (a superuser), anne, bob, and the sidecars' log; alt-1 to
// alt-4 switch between them. Each engine has its own database; switching engines reconnects the
// three psql panes to it.
(() => {
  const root = document.querySelector("[data-demo]");
  if (!root) return;

  const base = new URL(root.dataset.demo, document.baseURI);
  const at = (path) => new URL(path, base).href;
  const screen = root.querySelector(".pa-demo__screen");
  const boots = [...root.querySelectorAll("[data-boot]")];
  const status = root.querySelector(".pa-demo__status");
  const panes = [...root.querySelectorAll(".pa-demo__pane")];
  const engines = [...root.querySelectorAll("[data-engine]")];
  const acts = [...root.querySelectorAll(".pa-act")];
  const rail = root.querySelector(".pa-acts");
  const stepLists = [...root.querySelectorAll(".pa-demo__steps .pa-steps")];
  const PANE = { admin: "\x1b1", anne: "\x1b2", bob: "\x1b3", log: "\x1b4" };
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
        fontSize: matchMedia("(max-width: 40em)").matches ? 10 : 12,
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

      emulator.run();
      root.classList.add("is-running");
      setStatus("ready", "running in this tab");
      root._term = term;
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

  const showPane = (name) => panes.forEach((p) => p.classList.toggle("is-on", p.dataset.pane === name));

  // Reconnect the three psql panes to the engine's database, and come back to admin
  function connect(engine) {
    if (!ready()) return;
    for (const pane of ["anne", "bob", "admin"]) type(pane, `\\c ${engine}`);
    showPane("admin");
  }

  function pickEngine(engine) {
    if (engine === root.dataset.engine) return;
    root.dataset.engine = engine;
    root.dataset.family = FAMILY[engine];
    engines.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.engine === engine)));
    // another database: its steps start over
    root.querySelectorAll(".pa-step").forEach((s) => s.classList.remove("is-done"));
    markAct("protect");
    connect(engine);
  }

  // Light an act: its marker on the rail, the rail filled down to it, its steps in the frame
  function markAct(name) {
    root.dataset.act = name;
    acts.forEach((a) => a.classList.toggle("is-on", a.dataset.act === name));
    stepLists.forEach((l) => (l.hidden = l.dataset.act !== name));
    fillRail();
  }

  function fillRail() {
    const lit = acts.find((a) => a.dataset.act === root.dataset.act);
    const mark = lit?.querySelector(".pa-act__mark");
    if (!mark) return;
    const top = rail.getBoundingClientRect().top;
    const box = mark.getBoundingClientRect();
    rail.style.setProperty("--fill", `${box.top + box.height / 2 - top}px`);
  }

  async function send(step) {
    if (!started) await start();
    if (!ready()) return;
    type(step.dataset.pane, step.dataset.cmd);
    showPane(step.dataset.pane);
    step.classList.add("is-done");
    // the act's last step played: light the next act, whose steps take their place
    const list = step.closest(".pa-steps");
    const shown = [...list.querySelectorAll(".pa-step")].filter((s) => s.offsetParent);
    const next = acts[acts.findIndex((a) => a.dataset.act === list.dataset.act) + 1];
    if (next && shown.every((s) => s.classList.contains("is-done"))) {
      const focused = document.activeElement === step;
      markAct(next.dataset.act);
      // the played step is hidden with its act: keep the keyboard on the next act's first step
      if (focused) {
        const nextSteps = root.querySelector(`.pa-demo__steps [data-act="${next.dataset.act}"]`);
        [...nextSteps.querySelectorAll(".pa-step")].find((s) => s.offsetParent)?.focus();
      }
    }
  }

  boots.forEach((b) => b.addEventListener("click", start));
  engines.forEach((b) => b.addEventListener("click", () => pickEngine(b.dataset.engine)));
  root.querySelectorAll(".pa-step").forEach((s) => s.addEventListener("click", () => send(s)));
  acts.forEach((a) => a.querySelector(".pa-act__pick").addEventListener("click", () => markAct(a.dataset.act)));
  panes.forEach((p) =>
    p.addEventListener("click", () => {
      if (!ready()) return;
      emulator.console_input(PANE[p.dataset.pane]);
      showPane(p.dataset.pane);
    }),
  );
  // Scrolling marks nothing and types nothing: a statement runs only when the visitor asks for it
  new ResizeObserver(fillRail).observe(rail);
  fillRail();
})();
