defmodule Compare.Engines.Cerbos do
  @moduledoc """
  Cerbos v0.56.0 in a container, as in production: its policies on disk (a directory of YAML
  resource and derived role policies, mounted read-only), its HTTP API. Cerbos keeps no
  relationships: there is no model to load and no tuples to write; a scenario's
  `pg_auth/cerbos/` directory holds its policies (`:policies`).
  """
  @behaviour Compare.Engine

  @image "ghcr.io/cerbos/cerbos:0.56.0"

  alias Compare.Docker

  @impl true
  def name, do: "cerbos"

  @impl true
  def version(_), do: "cerbos " <> (@image |> String.split(":") |> List.last())

  @impl true
  def capabilities(_), do: []

  @impl true
  # Options: :policies (a directory of policies), :network (a Docker network to join), :server_args
  # (the server's command line instead of its defaults: compare/rules.exs stores policies through
  # the Admin API, in a database rather than the directory)
  def setup(opts) do
    args = opts[:server_args] || []
    network = ["--network", opts[:network] || raise("cerbos joins a stack's network (:network)")]
    env =
      case opts[:policies] do
        nil when args != [] -> network
        nil -> raise("cerbos needs a directory of policies (:policies) or :server_args")
        policies -> network ++ ["-v", "#{Path.expand(policies)}:/policies:ro"]
      end
    {name, port} = Docker.run(Docker.unique("compare-cerbos"), @image, 3592, args, env)
    state = %{container: name, db: nil, req: Req.new(base_url: "http://127.0.0.1:#{port}", retry: false), server: {args, env}}

    case wait(state.req, 120) do
      :ok -> {:ok, state}
      :error ->
        logs = Docker.logs(name)
        teardown(state)
        {:error, "cerbos did not become ready:\n#{logs}"}
    end
  end

  defp wait(_req, 0), do: :error

  defp wait(req, n) do
    case Req.get(req, url: "/_cerbos/health") do
      {:ok, %{status: 200}} -> :ok
      _ -> Process.sleep(500) && wait(req, n - 1)
    end
  end

  @impl true
  def teardown(state) do
    if state.container, do: Docker.stop(state.container)
    :ok
  end

  @impl true
  def fresh_server(%{server: {args, env}} = state) do
    Docker.stop(state.container)
    {name, port} = Docker.run(Docker.unique("compare-cerbos"), @image, 3592, args, env)
    req = Req.new(base_url: "http://127.0.0.1:#{port}", retry: false)
    if wait(req, 120) != :ok, do: raise("cerbos #{name} did not become ready")
    %{state | container: name, req: req}
  end

  @impl true
  def sidecar_env(state), do: [{"CERBOS_URL", "http://#{state.container}:3592"}]

  @doc """
  The ids of `resources` (%{id:, attr:}) the principal (%{id:, roles:, attr:}) may `action` on,
  from CheckResources (50 resources a call): the reference attribute rules are checked against.
  """
  def allowed(state, principal, kind, action, resources) do
    resources
    |> Enum.chunk_every(50)
    |> Enum.flat_map(fn batch ->
      body = %{requestId: "compare", principal: principal,
               resources: for(r <- batch, do: %{actions: [action], resource: %{kind: kind, id: r.id, attr: r.attr}})}
      %{status: 200, body: %{"results" => results}} = Req.post!(state.req, url: "/api/check/resources", json: body)
      for %{"resource" => %{"id" => id}, "actions" => %{^action => "EFFECT_ALLOW"}} <- results, do: id
    end)
    |> MapSet.new()
  end

  # no relationships: nothing to reset, load or write
  @impl true
  def reset(state), do: {:ok, state}
  @impl true
  def load_model(_state, _model), do: :ok
  @impl true
  def write_tuples(_state, []), do: {:ok, []}
  def write_tuples(_state, tuples), do: {:ok, Enum.map(tuples, &%{tuple: &1, error: "cerbos keeps no relationships"})}
  @impl true
  def staleness_ms(_), do: 0
  @impl true
  def revocation_ms(_), do: 0
  @impl true
  def check(_state, _op), do: {:unsupported, "cerbos decides on attributes"}
  @impl true
  def list_objects(_state, _op), do: {:unsupported, "cerbos lists nothing"}
  @impl true
  def list_users(_state, _op), do: {:unsupported, "cerbos lists nothing"}
end
