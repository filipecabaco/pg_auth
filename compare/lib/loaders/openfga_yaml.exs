defmodule Compare.Loader.OpenfgaYaml do
  @moduledoc """
  OpenFGA's consolidated test suite (assets/tests/*.yaml at a pinned commit) -> neutral scenarios.
  Pinned to OpenFGA v1.22.0 (commit a38d5d0), the server version compare runs; cached in compare/.cache.
  """
  @upstream "openfga/openfga"
  @tag "v1.22.0"
  @commit "a38d5d0f65a964b6030e0b1afd2a831f62309159"
  @files ["consolidated_1_1_tests.yaml", "abac_tests.yaml"]

  def source, do: %{repo: @upstream, tag: @tag, commit: @commit, files: @files}

  def load(opts) do
    scenarios = Enum.flat_map(@files, &load_file/1)

    case opts[:filter] do
      nil -> scenarios
      f -> Enum.filter(scenarios, &String.contains?(&1.name, f))
    end
  end

  defp load_file(file) do
    cache = Path.join([Path.dirname(__ENV__.file), "..", "..", ".cache", @commit]) |> Path.expand()
    File.mkdir_p!(cache)
    path = Path.join(cache, file)

    unless File.exists?(path) do
      url = "https://raw.githubusercontent.com/#{@upstream}/#{@commit}/assets/tests/#{file}"
      IO.puts("downloading #{url}")
      %{status: 200, body: body} = Req.get!(url, decode_body: false)
      File.write!(path, body)
    end

    {:ok, doc} = YamlElixir.read_from_file(path)
    prefix = if String.starts_with?(file, "abac"), do: "abac:", else: ""

    for t <- doc["tests"] do
      Compare.Scenario.finalize(%{
        name: prefix <> t["name"],
        source: "openfga-yaml",
        stages: Enum.map(t["stages"] || [], &stage/1)
      })
    end
  end

  defp stage(st) do
    checks =
      for a <- st["checkAssertions"] || [] do
        t = a["tuple"]

        %{
          kind: :check,
          subject: t["user"],
          relation: t["relation"],
          object: t["object"],
          contextual_tuples: ctuples(a["contextualTuples"]),
          context: a["context"] || %{},
          expect: expect(a, &(&1 == true))
        }
      end

    lo =
      for a <- st["listObjectsAssertions"] || [] do
        r = a["request"]

        %{
          kind: :list_objects,
          subject: r["user"],
          relation: r["relation"],
          type: r["type"],
          contextual_tuples: ctuples(a["contextualTuples"]),
          context: a["context"] || %{},
          expect: expect(a, &(&1 || []))
        }
      end

    lu =
      for a <- st["listUsersAssertions"] || [] do
        r = a["request"]

        %{
          kind: :list_users,
          object: r["object"],
          relation: r["relation"],
          filters: r["filters"] || [],
          contextual_tuples: ctuples(a["contextualTuples"]),
          context: a["context"] || %{},
          expect: expect(a, &(&1 || []))
        }
      end

    %{model: st["model"] || "", tuples: st["tuples"] || [], ops: checks ++ lo ++ lu}
  end

  defp ctuples(nil), do: []
  defp ctuples(%{"tupleKeys" => l}), do: l
  defp ctuples(l) when is_list(l), do: l

  defp expect(a, f) do
    if Map.has_key?(a, "errorCode"), do: {:error, a["errorCode"]}, else: {:value, f.(a["expectation"])}
  end
end
