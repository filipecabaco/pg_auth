defmodule Compare.Correctness do
  @moduledoc """
  Runs every operation of every scenario on every engine (engines in parallel, one scenario at a
  time), then classifies each operation:

    * per engine, against the scenario's expectation: pass | fail | no_expectation | unsupported
    * across engines: agree | disagree | single (fewer than two engines could answer)

  `unsupported` covers missing capabilities (conditions, contextual tuples, list operations, ...)
  and models an engine rejects while another accepts it (reason `model_rejected`).
  """
  alias Compare.{Engine, Scenario}

  # engines: [%{id:, mod:, state:, caps:}]
  def run(engines, scenarios, opts \\ []) do
    total = length(scenarios)

    {results, _} =
      scenarios
      |> Enum.with_index(1)
      |> Enum.map_reduce(engines, fn {scn, i}, engines ->
      if tty?(opts), do: IO.write("\r[#{i}/#{total}] #{String.slice(scn.name, 0, 60)}\e[K")

      ran =
        engines
        |> Task.async_stream(fn e -> {e, run_scenario(e, scn)} end, timeout: :infinity, ordered: true)
        |> Enum.map(fn {:ok, r} -> r end)

      per_engine = Map.new(ran, fn {e, {outcomes, _}} -> {e.id, outcomes} end)
      # reset/1 returns the state to use next (new database / store): thread it through
      engines = Enum.map(ran, fn {e, {_, state}} -> %{e | state: state} end)
      {classify(scn, engines, per_engine), engines}
    end)

    if tty?(opts), do: IO.puts("")
    List.flatten(results)
  end

  defp tty?(opts), do: not (opts[:quiet] || false) and match?({:ok, _}, :io.columns())

  # -> {%{op_id => outcome}, state}; outcome may also be {:model_error, msg}
  defp run_scenario(e, scn) do
    {:ok, state} = e.mod.reset(e.state)

    Enum.reduce(scn.stages, {%{}, state}, fn st, {acc, state} ->
        missing = Enum.reject(st.requires, &(&1 in e.caps))

        stage_outcomes =
          cond do
            missing != [] ->
              for op <- st.ops, into: %{}, do: {op.id, {:unsupported, Enum.join(missing, "+")}}

            true ->
              case e.mod.load_model(state, st.model) do
                {:error, msg} ->
                  for op <- st.ops, into: %{}, do: {op.id, {:model_error, msg}}

                :ok ->
                  {:ok, _issues} = e.mod.write_tuples(state, st.tuples)
                  for op <- st.ops, into: %{}, do: {op.id, Engine.run_op(e.mod, state, e.caps, op)}
              end
          end

        {Map.merge(acc, stage_outcomes), state}
      end)
  end

  defp classify(scn, engines, per_engine) do
    for st <- scn.stages, op <- st.ops do
      outs = Map.new(engines, fn e -> {e.id, per_engine[e.id][op.id]} end)
      any_ok = Enum.any?(outs, fn {_, o} -> match?({:ok, _}, o) end)

      # a rejected model counts as unsupported when another engine accepted it
      outs =
        Map.new(outs, fn
          {id, {:model_error, m}} when any_ok -> {id, {:unsupported, "model_rejected: " <> String.slice(m, 0, 160)}}
          {id, {:model_error, m}} -> {id, {:error, "model rejected: " <> String.slice(m, 0, 160)}}
          other -> other
        end)

      comparable = for {id, o} <- outs, match?({:ok, _}, o) or match?({:error, _}, o), do: {id, Scenario.normalize(o)}

      verdict =
        case comparable |> Enum.map(&elem(&1, 1)) |> Enum.uniq() do
          [] -> :single
          [_] when length(comparable) < 2 -> :single
          [_] -> :agree
          _ -> :disagree
        end

      vs_expect =
        Map.new(outs, fn
          {id, {:unsupported, _}} -> {id, :unsupported}
          {id, o} -> {id, expectation_status(o, op.expect)}
        end)

      %{
        id: op.id,
        scenario: scn.name,
        kind: op.kind,
        request: Map.drop(op, [:id, :kind, :expect, :requires]),
        expect: op.expect,
        outcomes: outs,
        verdict: verdict,
        vs_expect: vs_expect
      }
    end
  end

  defp expectation_status(_, nil), do: :no_expectation
  defp expectation_status({:error, _}, {:error, _}), do: :pass
  defp expectation_status(_, {:error, _}), do: :fail
  defp expectation_status({:error, _}, _), do: :fail
  defp expectation_status(o, {:value, _} = e), do: if(Scenario.normalize(o) == Scenario.normalize(e), do: :pass, else: :fail)
end
