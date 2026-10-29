defmodule Compare.Bench do
  @moduledoc """
  Same dataset on every engine, engines measured one after another (never concurrently). For each
  operation kind: untimed warmup, then `iters` calls cycling through the scenario's operations of
  that kind, once sequentially (latency, concurrency 1) and once with `concurrency` workers
  (throughput under load). Latency is wall time around the adapter call, so it includes each
  engine's transport: SQL over a Postgres connection vs HTTP+JSON.
  """
  alias Compare.Engine

  def run(engines, scn, opts) do
    iters = opts[:iters] || 500
    list_iters = opts[:list_iters] || max(div(iters, 5), 20)
    warmup = opts[:warmup] || 50
    conc = opts[:concurrency] || 8
    [stage] = scn.stages

    for e <- engines do
      IO.puts("\nbench #{e.id}: loading #{length(stage.tuples)} tuples")
      {:ok, state} = e.mod.reset(e.state)
      {t_model, res} = :timer.tc(fn -> e.mod.load_model(state, stage.model) end)
      if res != :ok, do: raise("#{e.id} rejected the benchmark model: #{inspect(res)}")
      {t_write, {:ok, issues}} = :timer.tc(fn -> e.mod.write_tuples(state, stage.tuples) end)

      load = %{
        model_ms: ms(t_model),
        tuples: length(stage.tuples),
        write_ms: ms(t_write),
        tuples_per_s: round(length(stage.tuples) / max(t_write / 1_000_000, 1.0e-6)),
        refused_tuples: length(issues)
      }

      kinds =
        for kind <- [:check, :list_objects, :list_users], into: %{} do
          ops = Enum.filter(stage.ops, &(&1.kind == kind))
          n = if kind == :check, do: iters, else: list_iters

          if Engine.missing(%{kind: kind}, e.caps) != [] do
            {kind, %{unsupported: "capability #{kind} missing"}}
          else
            IO.puts("  #{kind}: #{n} calls, #{length(ops)} distinct ops")
            call = fn i -> Engine.run_op(e.mod, state, e.caps, Enum.at(ops, rem(i, length(ops)))) end
            {kind, measure(call, n, warmup, conc)}
          end
        end

      %{engine: e.id, version: e.mod.version(e.state), load: load, kinds: kinds}
    end
  end

  def measure(call, n, warmup, conc) do
    for i <- 0..(warmup - 1)//1, do: call.(i)

    seq = timed(call, n, 1)
    par = timed(call, n, conc)
    %{sequential: seq, concurrent: Map.put(par, :concurrency, conc)}
  end

  defp timed(call, n, conc) do
    {wall, results} =
      :timer.tc(fn ->
        0..(n - 1)
        |> Task.async_stream(
          fn i ->
            {t, r} = :timer.tc(fn -> call.(i) end)
            {t, match?({:ok, _}, r)}
          end,
          max_concurrency: conc,
          timeout: :infinity,
          ordered: false
        )
        |> Enum.map(fn {:ok, x} -> x end)
      end)

    lat = results |> Enum.map(&elem(&1, 0)) |> Enum.sort()
    errors = Enum.count(results, fn {_, ok} -> not ok end)

    %{
      calls: n,
      errors: errors,
      p50_ms: pct(lat, 50),
      p95_ms: pct(lat, 95),
      p99_ms: pct(lat, 99),
      mean_ms: Float.round(Enum.sum(lat) / n / 1000, 3),
      ops_per_s: round(n / (wall / 1_000_000))
    }
  end

  defp pct(sorted, p) do
    idx = max(ceil(length(sorted) * p / 100) - 1, 0)
    Float.round(Enum.at(sorted, idx) / 1000, 3)
  end

  defp ms(us), do: round(us / 1000)
end
