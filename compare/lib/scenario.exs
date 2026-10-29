defmodule Compare.Scenario do
  @moduledoc """
  The neutral scenario: plain maps, so loaders and adapters need no shared structs.

      %{name: "folders", source: "openfga-yaml", stages: [stage]}
      stage = %{model: dsl, tuples: [tuple], ops: [op], requires: [atom]}
      tuple = %{object:, relation:, user:, condition: nil | %{name:, context:}}
      op    = %{id:, kind:, ..., contextual_tuples: [tuple], context: map,
                expect: nil | {:value, term} | {:error, code | nil}, requires: [atom]}

  A scenario is a list of stages because OpenFGA's suite replaces the model between stages while
  tuples persist. Tuples of a stage are written after its model is loaded.
  """

  @doc "Fill in derived fields (ids, requires, defaults). Loaders call this last."
  def finalize(%{name: name, stages: stages} = scn) do
    stages =
      stages
      |> Enum.with_index()
      |> Enum.map(fn {st, i} ->
        tuples = Enum.map(st[:tuples] || [], &tuple/1)
        model = st.model

        stage_req =
          [
            if(Regex.match?(~r/^\s*module\b/m, model), do: :modular_models),
            if(Regex.match?(~r/^condition\s/m, model) or Regex.match?(~r/\bwith\s+\w+/, model) or
                 Enum.any?(tuples, & &1.condition), do: :conditions)
          ]
          |> Enum.reject(&is_nil/1)

        ops =
          st[:ops]
          |> Kernel.||([])
          |> Enum.with_index()
          |> Enum.map(fn {op, j} -> finalize_op(op, "#{name}:s#{i}:#{op.kind}#{j}", stage_req) end)

        %{model: model, tuples: tuples, ops: ops, requires: stage_req}
      end)

    %{scn | stages: stages}
  end

  def tuple(t) do
    cond = t[:condition] || t["condition"]

    %{
      object: t[:object] || t["object"],
      relation: t[:relation] || t["relation"],
      user: t[:user] || t["user"],
      condition:
        case cond do
          nil -> nil
          c -> %{name: c[:name] || c["name"], context: c[:context] || c["context"] || %{}}
        end
    }
  end

  defp finalize_op(op, id, stage_req) do
    ctx = op[:context] || %{}
    ctuples = Enum.map(op[:contextual_tuples] || [], &tuple/1)

    req =
      [
        if(ctuples != [], do: :contextual_tuples),
        if(map_size(ctx) > 0, do: :context),
        if(op.kind == :check and String.contains?(op.subject, "#"), do: :userset_subject),
        if(Enum.any?(ctuples, & &1.condition), do: :conditions)
      ]
      |> Enum.reject(&is_nil/1)

    op
    |> Map.merge(%{id: id, contextual_tuples: ctuples, context: ctx, expect: op[:expect]})
    |> Map.put(:requires, Enum.uniq(req ++ stage_req))
  end

  @doc "Canonical form of an outcome (or expectation) for comparison."
  def normalize({:ok, list}) when is_list(list), do: {:ok, MapSet.new(list)}
  def normalize({:value, list}) when is_list(list), do: {:ok, MapSet.new(list)}
  def normalize({:value, v}), do: {:ok, v}
  def normalize({:ok, v}), do: {:ok, v}
  def normalize({:error, _}), do: :error
  def normalize(other), do: other

  @doc "Human readable outcome for reports."
  def show({:ok, v}) when is_boolean(v), do: to_string(v)
  def show({:ok, l}) when is_list(l), do: "[" <> Enum.join(Enum.sort(l), ", ") <> "]"
  def show({:error, m}), do: "error: " <> String.slice(to_string(m), 0, 120)
  def show({:unsupported, r}), do: "unsupported (#{r})"
  def show({:value, v}), do: show({:ok, v})
  def show(other), do: inspect(other)

  def op_count(scn), do: scn.stages |> Enum.map(&length(&1.ops)) |> Enum.sum()
end
