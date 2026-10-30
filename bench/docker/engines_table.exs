#!/usr/bin/env elixir
# Every engine against best-practice RLS, from a run on OpenFGA (whose results also hold the
# pg_auth_cerbos and pg_auth_cedar variants, each on its own engine) and a run on SpiceDB:
# docs/benchmarks.md's "All engines". Planning plus execution per query and subject pair, as
# pg_auth_table.exs measures it.
#
#   elixir bench/docker/engines_table.exs <openfga results dir> <spicedb results dir>

defmodule EnginesTable do
  # engine, the run it is in, the variant measuring it
  @engines [{"OpenFGA", :openfga, "pg_auth"}, {"SpiceDB", :spicedb, "pg_auth"}, {"Cerbos", :openfga, "pg_auth_cerbos"}, {"Cedar", :openfga, "pg_auth_cedar"}]

  def run([openfga, spicedb]) do
    runs = %{openfga: load(openfga), spicedb: load(spicedb)}

    # per engine and scenario: its (ratio, same rows) pairs against best-practice RLS
    measured =
      for {name, run, variant} <- @engines, {scenario, r} <- runs[run], pairs = pairs(r, variant), pairs != [], into: %{} do
        {{name, scenario}, pairs}
      end

    IO.puts("| engine | decides on | scenarios | pairs | vs best-practice RLS | faster in | same rows |")
    IO.puts("|---|---|---|---|---|---|---|")

    for {name, _, _} <- @engines do
      mine = for {{^name, scenario}, pairs} <- measured, do: {scenario, pairs}
      all = Enum.flat_map(mine, &elem(&1, 1))
      ratios = Enum.map(all, &elem(&1, 0))

      IO.puts(
        "| #{name} | #{decides(name)} | #{length(mine)} | #{length(all)} | #{fmt(geomean(ratios))} | #{Enum.count(ratios, &(&1 < 1))} | " <>
          "#{Enum.count(all, &elem(&1, 1))}/#{length(all)} |"
      )
    end

    # the scenarios more than the relationship engines run: every engine side by side
    shared = measured |> Map.keys() |> Enum.filter(fn {name, _} -> name in ["Cerbos", "Cedar"] end) |> Enum.map(&elem(&1, 1)) |> Enum.uniq() |> Enum.sort()
    names = Enum.map(@engines, &elem(&1, 0))
    IO.puts("\n| scenario | " <> Enum.join(names, " | ") <> " |")
    IO.puts("|---|" <> String.duplicate("---|", length(names)))

    for scenario <- shared do
      cells = for name <- names, do: measured[{name, scenario}] |> then(&if(&1, do: fmt(geomean(Enum.map(&1, fn {r, _} -> r end))), else: "-"))
      IO.puts("| `#{scenario}` | " <> Enum.join(cells, " | ") <> " |")
    end
  end

  def run(_), do: IO.puts(:stderr, "usage: engines_table.exs <openfga results dir> <spicedb results dir>") && System.halt(1)

  defp decides("Cerbos"), do: "the row's fields"
  defp decides("Cedar"), do: "fields and parents"
  defp decides(_), do: "relationships"

  defp load(dir) do
    for file <- Path.wildcard(Path.join(dir, "*.json")),
        run = JSON.decode!(File.read!(file)),
        is_map(run) and is_list(run["variants"]),
        into: %{},
        do: {run["scenario"], run}
  end

  # planning plus execution of `variant` against best-practice RLS, with whether the rows match
  defp pairs(run, variant) do
    by = for v <- run["variants"], r <- v["results"], r["exec_ms"], into: %{}, do: {{v["name"], r["subject"], r["query"]}, r}
    time = fn r -> (r["plan_ms"] || 0) + r["exec_ms"] end
    for {{^variant, s, q}, a} <- by, b = by[{"best_practice_rls", s, q}], do: {time.(a) / max(time.(b), 0.01), a["result_hash"] == b["result_hash"]}
  end

  defp geomean([]), do: nil
  defp geomean(xs), do: :math.exp(Enum.sum(Enum.map(xs, &:math.log(max(&1, 0.001)))) / length(xs))

  defp fmt(nil), do: "-"
  defp fmt(x), do: :erlang.float_to_binary(x * 1.0, decimals: 2) <> "x"
end

EnginesTable.run(System.argv())
