defmodule Compare.Engines.Openfga do
  @moduledoc """
  OpenFGA v1.22.0 in a container, set up as in production: a migrated Postgres datastore, its
  check and list caches on, and a connection pool sized to the database. Spoken to over its HTTP API. The server takes
  JSON models only, so DSL text is converted with the official `openfga/cli` image
  (`fga model transform`), cached on disk under compare/.cache/models by content hash.
  """
  @behaviour Compare.Engine

  @image "openfga/openfga:v1.22.0"
  @cli "openfga/cli:latest"
  @caps [:check, :list_objects, :list_users, :contextual_tuples, :context, :conditions, :wildcards, :userset_subject]

  alias Compare.Docker

  @impl true
  def name, do: "openfga"

  @impl true
  def version(_), do: "openfga " <> (@image |> String.split(":") |> List.last())

  @impl true
  def capabilities(_), do: @caps

  # caches OpenFGA leaves off by default; its production guide recommends the check cache
  @caches ~w(OPENFGA_CHECK_QUERY_CACHE_ENABLED OPENFGA_CHECK_ITERATOR_CACHE_ENABLED
             OPENFGA_LIST_OBJECTS_ITERATOR_CACHE_ENABLED OPENFGA_CACHE_CONTROLLER_ENABLED)
  # the datastore's max_connections is 200: half of it, kept open (OpenFGA defaults to 30 open, 10 idle)
  @pool ["-e", "OPENFGA_DATASTORE_MAX_OPEN_CONNS=100", "-e", "OPENFGA_DATASTORE_MAX_IDLE_CONNS=100"]

  @impl true
  # Options (the CLI's options are passed too, and ignored):
  #   :network     - a Docker network to join (compare/lib/stack.exs); otherwise one is created
  #   :docker_args - extra `docker run` arguments for the server (environment)
  def setup(opts) do
    id = Docker.unique("compare-openfga")
    own_net = is_nil(opts[:network])
    net = opts[:network] || id
    state = %{container: nil, db: nil, network: if(own_net, do: net), req: nil, store: nil, agent: nil}

    try do
      if own_net, do: {_, 0} = System.cmd("docker", ["network", "create", "--label", "pg_auth_compare=1", net], stderr_to_stdout: true)
      state = %{state | db: start_datastore(id <> "-db", net)}
      args = ["run", "--datastore-engine", "postgres", "--datastore-uri", datastore_uri(state.db)]
      env = ["--network", net] ++ @pool ++ Enum.flat_map(@caches, &["-e", "#{&1}=true"]) ++ (opts[:docker_args] || [])

      {name, port} = Docker.run(id, @image, 8080, args, env)
      state = %{state | container: name}
      req = Req.new(base_url: "http://127.0.0.1:#{port}", retry: false, receive_timeout: 120_000)

      case wait(req, 60) do
        :ok ->
          {:ok, Map.merge(state, %{req: req, server: {args, env}})}

        :error ->
          teardown(state)
          {:error, "openfga did not become healthy"}
      end
    rescue
      e ->
        teardown(state)
        reraise e, __STACKTRACE__
    end
  end

  defp datastore_uri(db), do: "postgres://postgres:openfga@#{db}:5432/postgres?sslmode=disable"

  # A Postgres container for OpenFGA, migrated.
  defp start_datastore(name, net) do
    {_, 0} =
      System.cmd("docker", ["run", "-d", "--label", "pg_auth_compare=1", "--name", name, "--network", net,
                            "-e", "POSTGRES_PASSWORD=openfga", "postgres:18", "-c", "max_connections=200",
                            "-c", "shared_buffers=512MB"], stderr_to_stdout: true)

    # the image's init runs a socket-only server first: wait for TCP
    ready = fn -> match?({_, 0}, System.cmd("docker", ["exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end
    Enum.find_value(1..120, fn _ -> ready.() || (Process.sleep(500) && nil) end) || raise("openfga datastore #{name} never became ready")

    case System.cmd("docker", ["run", "--rm", "--network", net, @image, "migrate", "--datastore-engine", "postgres", "--datastore-uri", datastore_uri(name)], stderr_to_stdout: true) do
      {_, 0} -> name
      {out, _} -> raise "openfga migrate failed: #{out}"
    end
  end

  defp wait(_req, 0), do: :error

  defp wait(req, n) do
    case Req.get(req, url: "/healthz") do
      {:ok, %{status: 200}} ->
        :ok

      _ ->
        Process.sleep(500)
        wait(req, n - 1)
    end
  end

  @doc """
  Replace the server with a new container on the same datastore: the store,
  model and tuples stay, the server's caches start empty. -> the new state.
  """
  @impl true
  def fresh_server(%{server: {args, env}} = state) do
    Docker.stop(state.container)
    {name, port} = Docker.run(Docker.unique("compare-openfga"), @image, 8080, args, env)
    req = Req.new(base_url: "http://127.0.0.1:#{port}", retry: false, receive_timeout: 120_000)
    if wait(req, 60) != :ok, do: raise("openfga #{name} did not become healthy")
    %{state | container: name, req: req}
  end

  @doc "Environment for a pg-auth-sidecar on the same network."
  @impl true
  def sidecar_env(state) do
    [{"OPENFGA_URL", "http://#{state.container}:8080"}, {"OPENFGA_STORE_ID", state.store}, {"OPENFGA_MODEL_ID", model_id(state)}]
  end

  @doc """
  How long after a write the engine may still answer from before it: its check and list caches'
  TTL (10 s) when they are on, for a question asked before the write.
  """
  @impl true
  def staleness_ms(_), do: 0
  @impl true
  def revocation_ms(_), do: 10_000

  @doc "Delete tuples (%{object:, relation:, user:})."
  @impl true
  def delete_tuples(state, tuples) do
    body = %{"deletes" => %{"tuple_keys" => Enum.map(tuples, &Map.take(key(&1), ["object", "relation", "user"]))}, "authorization_model_id" => model_id(state)}

    case Req.post(state.req, url: "/stores/#{state.store}/write", json: body) do
      {:ok, %{status: 200}} -> :ok
      {:ok, r} -> {:error, msg(r)}
      {:error, e} -> {:error, Exception.message(e)}
    end
  end

  @impl true
  def teardown(state) do
    for c <- [state.container, state.db], c, do: Docker.stop(c)
    if state.network, do: System.cmd("docker", ["network", "rm", state.network], stderr_to_stdout: true)
    :ok
  end

  @impl true
  def reset(state) do
    if state.agent, do: Agent.stop(state.agent)
    %{status: 201, body: %{"id" => id}} = Req.post!(state.req, url: "/stores", json: %{name: "compare"})
    {:ok, agent} = Agent.start(fn -> nil end)
    {:ok, %{state | store: id, agent: agent}}
  end

  @impl true
  def load_model(state, dsl) do
    with {:ok, json} <- to_json(dsl) do
      case Req.post(state.req, url: "/stores/#{state.store}/authorization-models", json: json) do
        {:ok, %{status: 201, body: %{"authorization_model_id" => id}}} ->
          Agent.update(state.agent, fn _ -> id end)
          :ok

        {:ok, r} ->
          {:error, msg(r)}

        {:error, e} ->
          {:error, Exception.message(e)}
      end
    end
  end

  @impl true
  def write_tuples(state, tuples) do
    issues =
      tuples
      |> Enum.chunk_every(100)
      |> Enum.flat_map(fn batch ->
        case write(state, batch) do
          :ok -> []
          {:error, _} -> one_by_one(state, batch)
        end
      end)

    {:ok, issues}
  end

  defp one_by_one(state, batch) do
    for t <- batch, {:error, m} <- [write(state, [t])], do: %{tuple: t, error: m}
  end

  defp write(state, batch) do
    body = %{
      "writes" => %{"tuple_keys" => Enum.map(batch, &key/1), "on_duplicate" => "ignore"},
      "authorization_model_id" => model_id(state)
    }

    case Req.post(state.req, url: "/stores/#{state.store}/write", json: body) do
      {:ok, %{status: 200}} -> :ok
      {:ok, r} -> {:error, msg(r)}
      {:error, e} -> {:error, Exception.message(e)}
    end
  end

  defp model_id(state), do: Agent.get(state.agent, & &1)

  defp key(t) do
    base = %{"object" => t.object, "relation" => t.relation, "user" => t.user}

    case t.condition do
      nil -> base
      c -> Map.put(base, "condition", %{"name" => c.name, "context" => c.context || %{}})
    end
  end

  # list-users takes a bare array of tuple keys; check and list-objects wrap them in {tuple_keys: [...]}
  defp ctx_fields(state, op) do
    wrap = if op.kind == :list_users, do: & &1, else: &%{"tuple_keys" => &1}

    %{"authorization_model_id" => model_id(state)}
    |> put_unless_empty("contextual_tuples", op.contextual_tuples, &wrap.(Enum.map(&1, fn t -> key(t) end)))
    |> put_unless_empty("context", op.context, & &1)
  end

  defp put_unless_empty(m, _k, v, _f) when v in [nil, [], %{}], do: m
  defp put_unless_empty(m, k, v, f), do: Map.put(m, k, f.(v))

  @impl true
  def check(state, op) do
    body =
      Map.merge(ctx_fields(state, op), %{
        "tuple_key" => %{"user" => op.subject, "relation" => op.relation, "object" => op.object}
      })

    case post(state, "check", body) do
      {:ok, %{"allowed" => a}} -> {:ok, a}
      {:error, m} -> {:error, m}
    end
  end

  @impl true
  def list_objects(state, op) do
    body = Map.merge(ctx_fields(state, op), %{"user" => op.subject, "relation" => op.relation, "type" => op.type})
    # a read that bypasses the caches
    body = if op[:consistency] == :strong, do: Map.put(body, "consistency", "HIGHER_CONSISTENCY"), else: body

    case post(state, "list-objects", body) do
      {:ok, %{"objects" => objs}} -> {:ok, objs}
      {:error, m} -> {:error, m}
    end
  end

  @impl true
  def list_users(state, op) do
    [type, id] = String.split(op.object, ":", parts: 2)

    filters =
      for f <- op.filters do
        case String.split(f, "#", parts: 2) do
          [t] -> %{"type" => t}
          [t, r] -> %{"type" => t, "relation" => r}
        end
      end

    body =
      Map.merge(ctx_fields(state, op), %{
        "object" => %{"type" => type, "id" => id},
        "relation" => op.relation,
        "user_filters" => filters
      })

    case post(state, "list-users", body) do
      {:ok, %{"users" => users}} -> {:ok, Enum.map(users, &user/1)}
      {:error, m} -> {:error, m}
    end
  end

  defp user(%{"object" => %{"type" => t, "id" => i}}), do: "#{t}:#{i}"
  defp user(%{"userset" => %{"type" => t, "id" => i, "relation" => r}}), do: "#{t}:#{i}##{r}"
  defp user(%{"wildcard" => %{"type" => t}}), do: "#{t}:*"

  defp post(state, path, body) do
    case Req.post(state.req, url: "/stores/#{state.store}/#{path}", json: body) do
      {:ok, %{status: 200, body: b}} -> {:ok, b}
      {:ok, r} -> {:error, msg(r)}
      {:error, e} -> {:error, Exception.message(e)}
    end
  end

  defp msg(%{status: s, body: %{"code" => c, "message" => m}}), do: "#{s} #{c}: #{m}"
  defp msg(%{status: s, body: b}), do: "#{s} #{inspect(b)}"

  # ---- DSL -> JSON --------------------------------------------------------------------------

  @cache Path.expand("../../.cache/models", __DIR__)

  @impl true
  def prepare(_state, scenarios) do
    unless Docker.image?(@cli), do: System.cmd("docker", ["pull", @cli], stderr_to_stdout: true)

    todo =
      for s <- scenarios, st <- s.stages, st.model != "", uniq: true, do: st.model

    todo = Enum.reject(todo, &File.exists?(path(&1)))

    if todo != [] do
      IO.puts("openfga: converting #{length(todo)} models DSL -> JSON with #{@cli} (cached afterwards)")

      todo
      |> Task.async_stream(&transform/1, max_concurrency: 8, timeout: :infinity)
      |> Stream.run()
    end

    :ok
  end

  defp path(dsl), do: Path.join(@cache, Base.encode16(:crypto.hash(:sha256, dsl), case: :lower) <> ".json")

  defp to_json(dsl) do
    case File.read(path(dsl)) do
      {:ok, body} -> {:ok, JSON.decode!(body)}
      _ -> transform(dsl)
    end
  end

  defp transform(dsl) do
    File.mkdir_p!(@cache)

    case System.cmd("docker", ["run", "--rm", @cli, "model", "transform", dsl, "--input-format", "fga"], stderr_to_stdout: true) do
      {out, 0} ->
        File.write!(path(dsl), out)
        {:ok, JSON.decode!(out)}

      {out, _} ->
        {:error, "model transform failed: " <> String.slice(out, 0, 300)}
    end
  end
end
