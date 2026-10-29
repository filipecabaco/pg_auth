#!/usr/bin/env elixir
# pg_auth against best-practice RLS, per scenario, from a run's results directory: the table in
# bench/docker/README.md ("Results").
#
#   elixir bench/docker/pg_auth_table.exs <results dir> [<later results dir> ...]
#
# A later directory's scenarios replace an earlier one's (scenarios rerun after a fix).

defmodule PgAuthTable do
  def run([]), do: IO.puts(:stderr, "usage: pg_auth_table.exs <results dir> [<later results dir> ...]") && System.halt(1)

  def run(dirs) do
    runs =
      Enum.reduce(dirs, %{}, fn dir, acc ->
        for file <- Path.wildcard(Path.join(dir, "*.json")), run = JSON.decode!(File.read!(file)), pg_auth?(run), into: acc do
          {run["scenario"], run}
        end
      end)

    rows = for {scenario, run} <- Enum.sort(runs), do: {scenario, row(run)}
    pairs = Enum.flat_map(rows, fn {_, r} -> r.pairs end)

    IO.puts("Over all #{length(pairs)} pairs pg_auth is at #{fmt(geomean(Enum.map(pairs, &elem(&1, 0))))} of best-practice RLS, and faster in #{Enum.count(pairs, &(elem(&1, 0) < 1))} of them.\n")
    IO.puts("| scenario | pg_auth vs best-practice RLS | worst query | reused plan vs fresh | same rows |")
    IO.puts("|---|---|---|---|---|")

    for {scenario, r} <- rows do
      ratios = Enum.map(r.pairs, &elem(&1, 0))
      same = Enum.count(r.pairs, &elem(&1, 1))
      IO.puts("| `#{scenario}` | #{fmt(geomean(ratios))} | #{fmt(Enum.max(ratios, fn -> nil end))} | #{fmt(geomean(r.reused))} | #{same}/#{length(r.pairs)} |")
    end
  end

  defp pg_auth?(run), do: is_map(run) and is_list(run["variants"]) and Enum.any?(run["variants"], &(&1["name"] == "pg_auth"))

  # planning plus execution, per (variant, subject, query)
  defp row(run) do
    by = for v <- run["variants"], r <- v["results"], r["exec_ms"], into: %{}, do: {{v["name"], r["subject"], r["query"]}, r}
    time = fn r -> (r["plan_ms"] || 0) + r["exec_ms"] end

    %{
      pairs: for({{"pg_auth", s, q}, a} <- by, b = by[{"best_practice_rls", s, q}], do: {time.(a) / max(time.(b), 0.01), a["result_hash"] == b["result_hash"]}),
      reused: for({{"pg_auth_reused_plan", s, q}, a} <- by, b = by[{"pg_auth", s, q}], do: time.(a) / max(time.(b), 0.01))
    }
  end

  defp geomean([]), do: nil
  defp geomean(xs), do: :math.exp(Enum.sum(Enum.map(xs, &:math.log(max(&1, 0.001)))) / length(xs))

  defp fmt(nil), do: "-"
  defp fmt(x), do: :erlang.float_to_binary(x * 1.0, decimals: 2) <> "x"
end

PgAuthTable.run(System.argv())
