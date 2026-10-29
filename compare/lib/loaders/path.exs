defmodule Compare.Loader.Path do
  @moduledoc """
  Hand-written scenarios: a YAML file (or a directory of them) in the neutral format.

      name: docs-basic
      stages:
        - model: |
            model
              schema 1.1
            type user
            type document
              relations
                define viewer: [user]
          tuples:
            - {object: "document:1", relation: viewer, user: "user:anne"}
          operations:
            - {op: check, subject: "user:anne", relation: viewer, object: "document:1", expect: true}
            - {op: check, ..., contextual_tuples: [...], context: {x: 1}, expect: false}
            - {op: check, ..., expect_error: true}
            - {op: list_objects, subject: "user:anne", relation: viewer, type: document, expect: ["document:1"]}
            - {op: list_users, object: "document:1", relation: viewer, filters: [user], expect: ["user:anne"]}

  Omit `expect` to compare engines against each other only.
  """

  def load(path) do
    files =
      if File.dir?(path),
        do: path |> Elixir.Path.join("*.{yaml,yml}") |> Path.wildcard() |> Enum.sort(),
        else: [path]

    Enum.map(files, &load_file/1)
  end

  defp load_file(file) do
    {:ok, doc} = YamlElixir.read_from_file(file)

    Compare.Scenario.finalize(%{
      name: doc["name"] || Elixir.Path.basename(file, Elixir.Path.extname(file)),
      source: "path:" <> Elixir.Path.basename(file),
      stages:
        for st <- doc["stages"] do
          %{model: st["model"], tuples: st["tuples"] || [], ops: Enum.map(st["operations"] || [], &op/1)}
        end
    })
  end

  defp op(o) do
    base = %{
      kind: String.to_existing_atom(o["op"]),
      contextual_tuples: o["contextual_tuples"] || [],
      context: o["context"] || %{},
      expect: expect(o)
    }

    case base.kind do
      :check ->
        Map.merge(base, %{subject: o["subject"], relation: o["relation"], object: o["object"]})

      :list_objects ->
        Map.merge(base, %{subject: o["subject"], relation: o["relation"], type: o["type"]})

      :list_users ->
        Map.merge(base, %{object: o["object"], relation: o["relation"], filters: o["filters"] || []})
    end
  end

  defp expect(%{"expect_error" => true}), do: {:error, nil}
  defp expect(%{"expect" => e}), do: {:value, e}
  defp expect(_), do: nil
end
