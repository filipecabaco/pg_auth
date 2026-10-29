#!/usr/bin/env elixir
# Engine-agnostic authorization comparison. See compare/README.md.
#
#   elixir compare/run.exs --engines openfga --suite openfga-yaml
#   elixir compare/run.exs --engines openfga --suite random --seed 7 --ops 500
#   elixir compare/run.exs --suite path --path compare/scenarios
#   elixir compare/run.exs --bench --scale 5 --iters 1000

Mix.install(
  [{:req, "~> 0.7"}, {:yaml_elixir, "~> 2.12"}]
)

dir = Path.join(__DIR__, "lib")

for f <- ["engine", "docker", "scenario", "correctness", "bench", "report"] ++
           (Path.wildcard(Path.join(dir, "loaders/*.exs")) ++ Path.wildcard(Path.join(dir, "engines/*.exs"))) do
  Code.require_file(if String.ends_with?(f, ".exs"), do: f, else: Path.join(dir, f <> ".exs"))
end

defmodule Compare.Main do
  alias Compare.{Bench, Correctness, Engine, Report}

  @strict [
    engines: :string, suite: :string, path: :string, seed: :integer, ops: :integer, scale: :integer,
    bench: :boolean, iters: :integer, list_iters: :integer, warmup: :integer, concurrency: :integer,
    filter: :string, name: :string
  ]

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: @strict)
    ids = (opts[:engines] || "openfga") |> String.split(",", trim: true)
    suite = opts[:suite] || if(opts[:bench], do: "random", else: "openfga-yaml")
    mods = Map.new(ids, &{&1, Engine.module_for(&1)})
    scenarios = load(suite, opts)
    stamp = DateTime.utc_now() |> Calendar.strftime("%Y%m%d-%H%M%S")
    run = opts[:name] || "#{if opts[:bench], do: "bench", else: "correctness"}-#{suite}-#{stamp}"

    IO.puts("suite #{suite}: #{length(scenarios)} scenarios, #{Enum.sum(Enum.map(scenarios, &Compare.Scenario.op_count/1))} operations")
    engines = start(ids, mods, opts)

    try do
      for e <- engines, function_exported?(e.mod, :prepare, 2), do: e.mod.prepare(e.state, scenarios)

      if opts[:bench],
        do: bench(engines, scenarios, opts, run),
        else: correctness(engines, scenarios, opts, suite, run)
    after
      for e <- engines, do: e.mod.teardown(e.state)
    end
  end

  defp load("openfga-yaml", opts), do: Compare.Loader.OpenfgaYaml.load(opts)
  defp load("random", opts), do: Compare.Loader.Random.load(opts)
  defp load("path", opts), do: Compare.Loader.Path.load(opts[:path] || Path.join(__DIR__, "scenarios"))
  defp load(other, _), do: raise("unknown suite #{other}; use openfga-yaml, random or path")

  defp start(ids, mods, opts) do
    started =
      ids
      |> Task.async_stream(
        fn id ->
          IO.puts("starting #{id}")
          {id, mods[id].setup(opts)}
        end,
        timeout: :infinity
      )
      |> Enum.map(fn {:ok, r} -> r end)

    case Enum.find(started, &match?({_, {:error, _}}, &1)) do
      nil ->
        for {id, {:ok, state}} <- started do
          mod = mods[id]
          %{id: id, mod: mod, state: state, caps: mod.capabilities(state)}
        end

      {id, {:error, why}} ->
        for {i, {:ok, st}} <- started, do: mods[i].teardown(st)
        IO.puts(:stderr, "engine #{id} failed to start: #{inspect(why)}")
        System.halt(2)
    end
  end

  defp correctness(engines, scenarios, opts, suite, run) do
    for e <- engines, do: IO.puts("#{e.id}: #{e.mod.version(e.state)}  capabilities #{inspect(e.caps)}")
    {t, results} = :timer.tc(fn -> Correctness.run(engines, scenarios, opts) end)

    meta = %{
      run: run,
      suite: suite,
      seed: opts[:seed],
      scale: opts[:scale],
      scenarios: length(scenarios),
      seconds: div(t, 1_000_000),
      source: if(suite == "openfga-yaml", do: Compare.Loader.OpenfgaYaml.source())
    }

    rep = Report.correctness(results, engines, meta)
    Report.print_correctness(rep)
    write(run, "json", JSON.encode!(Report.correctness_json(rep)))
    write(run, "md", Report.correctness_markdown(rep, run))
    if (rep.summary.verdicts[:disagree] || 0) > 0, do: System.halt(1)
  end

  defp bench(engines, scenarios, opts, run) do
    scn = hd(scenarios)

    if length(scenarios) != 1 or length(scn.stages) != 1,
      do: raise("--bench needs a single-stage scenario (use --suite random or a path to one file)")

    rows = Bench.run(engines, scn, opts)
    Report.print_bench(rows)

    meta = %{
      run: run,
      scenario: scn.name,
      dataset: scn[:dataset],
      iters: opts[:iters] || 500,
      concurrency: opts[:concurrency] || 8,
      warmup: opts[:warmup] || 50
    }

    write(run, "json", JSON.encode!(Report.bench_json(rows, meta)))
    write(run, "md", Report.bench_markdown(rows, meta))
  end

  defp write(run, ext, body) do
    out = Path.join([__DIR__, "results", "#{run}.#{ext}"])
    File.mkdir_p!(Path.dirname(out))
    File.write!(out, body)
    IO.puts("wrote #{Path.relative_to_cwd(out)}")
  end
end

Compare.Main.main(System.argv())
