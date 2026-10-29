defmodule Compare.Engines.Spicedb do
  @moduledoc """
  SpiceDB v1.56.2 in a container, set up as in production: a migrated Postgres datastore (with
  `track_commit_timestamp`, which Watch needs), its caches on (the default), a preshared key, and
  a connection pool sized to the database. Spoken to over its HTTP gateway (`--http-enabled`),
  the gRPC API as JSON, as the sidecar does.

  Models arrive as OpenFGA DSL and are translated to a SpiceDB schema
  (`Compare.Engines.Spicedb.Schema`); a relation that mixes directly assigned types with rules
  becomes a relation `<name>__direct` and a permission `<name>`, and tuples written to it go to
  the relation.
  """
  @behaviour Compare.Engine

  @image "authzed/spicedb:v1.56.2"
  @key "compare-preshared-key"
  @caps [:check, :list_objects, :wildcards, :userset_subject]
  # SpiceDB answers minimize_latency reads from a revision up to its quantization window old
  # (--datastore-revision-quantization-interval, 5 s) plus 10% (max-staleness-percent)
  @staleness_ms 5_500

  alias Compare.Docker
  alias Compare.Engines.Spicedb.Schema

  @impl true
  def name, do: "spicedb"

  @impl true
  def version(_), do: "spicedb " <> (@image |> String.split(":") |> List.last())

  @impl true
  def capabilities(_), do: @caps

  @doc "How long after a write the engine's default reads may still answer from before it."
  @impl true
  def staleness_ms(_), do: @staleness_ms
  @impl true
  def revocation_ms(_), do: @staleness_ms

  # the datastore's max_connections is 200: half of it, as OpenFGA's pool
  @pool ~w(--datastore-conn-pool-read-max-open 80 --datastore-conn-pool-read-min-open 80
           --datastore-conn-pool-write-max-open 20 --datastore-conn-pool-write-min-open 20)

  @impl true
  # Options: :network (a Docker network to join; otherwise one is created), :docker_args
  def setup(opts) do
    id = Docker.unique("compare-spicedb")
    own_net = is_nil(opts[:network])
    net = opts[:network] || id
    state = %{container: nil, db: nil, network: if(own_net, do: net), req: nil, server: nil, agent: nil}

    try do
      if own_net, do: {_, 0} = System.cmd("docker", ["network", "create", "--label", "pg_auth_compare=1", net], stderr_to_stdout: true)
      state = %{state | db: start_datastore(id <> "-db", net)}
      args = ["serve", "--grpc-preshared-key", @key, "--datastore-engine", "postgres", "--datastore-conn-uri", datastore_uri(state.db),
              "--http-enabled", "--http-addr", ":8443"] ++ @pool
      env = ["--network", net] ++ (opts[:docker_args] || [])
      {name, port} = Docker.run(id, @image, 8443, args, env)
      state = %{state | container: name, server: {args, env}}
      {:ok, agent} = Agent.start(fn -> %{} end)
      state = %{state | req: req(port), agent: agent}

      case wait(state.req, 120) do
        :ok ->
          {:ok, state}

        :error ->
          teardown(state)
          {:error, "spicedb did not become ready"}
      end
    rescue
      e ->
        teardown(state)
        reraise e, __STACKTRACE__
    end
  end

  defp req(port) do
    Req.new(base_url: "http://127.0.0.1:#{port}", auth: {:bearer, @key}, retry: false, receive_timeout: 120_000, decode_body: false)
  end

  defp datastore_uri(db), do: "postgres://postgres:spicedb@#{db}:5432/postgres?sslmode=disable"

  defp start_datastore(name, net) do
    {_, 0} =
      System.cmd("docker", ["run", "-d", "--label", "pg_auth_compare=1", "--name", name, "--network", net,
                            "-e", "POSTGRES_PASSWORD=spicedb", "postgres:18", "-c", "max_connections=200",
                            "-c", "shared_buffers=512MB", "-c", "track_commit_timestamp=on"], stderr_to_stdout: true)

    ready = fn -> match?({_, 0}, System.cmd("docker", ["exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end
    Enum.find_value(1..120, fn _ -> ready.() || (Process.sleep(500) && nil) end) || raise("spicedb datastore #{name} never became ready")

    case System.cmd("docker", ["run", "--rm", "--network", net, @image, "datastore", "migrate", "head", "--datastore-engine", "postgres",
                               "--datastore-conn-uri", datastore_uri(name)], stderr_to_stdout: true) do
      {_, 0} -> name
      {out, _} -> raise "spicedb migrate failed: #{out}"
    end
  end

  # ready once the API answers (with no schema yet: not found)
  defp wait(_req, 0), do: :error

  defp wait(req, n) do
    case Req.post(req, url: "/v1/schema/read", json: %{}) do
      {:ok, %{status: s}} when s in [200, 404] ->
        :ok

      _ ->
        Process.sleep(500)
        wait(req, n - 1)
    end
  end

  @doc "Replace the server with a new container on the same datastore: schema and relationships stay, its caches start empty."
  @impl true
  def fresh_server(%{server: {args, env}} = state) do
    Docker.stop(state.container)
    {name, port} = Docker.run(Docker.unique("compare-spicedb"), @image, 8443, args, env)
    req = req(port)
    if wait(req, 120) != :ok, do: raise("spicedb #{name} did not become ready")
    %{state | container: name, req: req}
  end

  @impl true
  def teardown(state) do
    for c <- [state.container, state.db], c, do: Docker.stop(c)
    if state.network, do: System.cmd("docker", ["network", "rm", state.network], stderr_to_stdout: true)
    if state.agent, do: Agent.stop(state.agent)
    :ok
  end

  @doc "Environment for a pg-auth-sidecar on the same network."
  @impl true
  def sidecar_env(state), do: [{"SPICEDB_URL", "http://#{state.container}:8443"}, {"SPICEDB_TOKEN", @key}]

  @impl true
  # every relationship of every definition, then the schema
  def reset(state) do
    with {:ok, %{"schemaText" => text}} <- post(state, "/v1/schema/read", %{}) do
      for [_, type] <- Regex.scan(~r/^definition ([a-z0-9_\/]+)/m, text) do
        {:ok, _} = post(state, "/v1/relationships/delete", %{relationship_filter: %{resource_type: type}})
      end
    end

    Agent.update(state.agent, fn _ -> %{} end)
    {:ok, state}
  end

  @impl true
  def load_model(state, dsl) do
    with {:ok, schema, renames} <- Schema.from_fga(dsl),
         {:ok, _} <- post(state, "/v1/schema/write", %{schema: schema}) do
      Agent.update(state.agent, fn _ -> renames end)
      :ok
    end
  end

  @impl true
  def write_tuples(state, tuples) do
    renames = Agent.get(state.agent, & &1)

    issues =
      tuples
      |> Enum.chunk_every(1000)
      |> Task.async_stream(fn batch -> {batch, write(state, batch, renames)} end, max_concurrency: 8, timeout: :infinity)
      |> Enum.flat_map(fn
        {:ok, {_, :ok}} -> []
        {:ok, {batch, {:error, _}}} -> for t <- batch, {:error, m} <- [write(state, [t], renames)], do: %{tuple: t, error: m}
      end)

    {:ok, issues}
  end

  @doc "Delete tuples (%{object:, relation:, user:})."
  @impl true
  def delete_tuples(state, tuples) do
    renames = Agent.get(state.agent, & &1)
    updates = for t <- tuples, do: %{operation: "OPERATION_DELETE", relationship: relationship(t, renames)}

    case post(state, "/v1/relationships/write", %{updates: updates}) do
      {:ok, _} -> :ok
      e -> e
    end
  end

  defp write(state, batch, renames) do
    updates = for t <- batch, do: %{operation: "OPERATION_TOUCH", relationship: relationship(t, renames)}

    case post(state, "/v1/relationships/write", %{updates: updates}) do
      {:ok, _} -> :ok
      e -> e
    end
  end

  defp relationship(%{condition: c}, _) when c != nil, do: raise("spicedb adapter: conditions are not translated")

  defp relationship(t, renames) do
    {type, id} = object(t.object)
    %{resource: %{object_type: type, object_id: id}, relation: Map.get(renames, {type, t.relation}, t.relation), subject: subject(t.user)}
  end

  defp object(o), do: o |> String.split(":", parts: 2) |> List.to_tuple()

  defp subject(user) do
    {o, rel} =
      case String.split(user, "#", parts: 2) do
        [o, r] -> {o, r}
        [o] -> {o, nil}
      end

    {type, id} = object(o)
    s = %{object: %{object_type: type, object_id: id}}
    if rel, do: Map.put(s, :optional_relation, rel), else: s
  end

  @impl true
  def check(state, op) do
    {type, id} = object(op.object)
    body = %{consistency: %{fully_consistent: true}, resource: %{object_type: type, object_id: id}, permission: op.relation, subject: subject(op.subject)}

    case post(state, "/v1/permissions/check", body) do
      {:ok, %{"permissionship" => p}} -> {:ok, p == "PERMISSIONSHIP_HAS_PERMISSION"}
      e -> e
    end
  end

  @impl true
  # every object at the latest revision (the parity reference): definite permissions only
  def list_objects(state, op) do
    body = %{consistency: %{fully_consistent: true}, resource_object_type: op.type, permission: op.relation, subject: subject(op.subject)}

    with {:ok, lines} <- stream(state, "/v1/permissions/resources", body) do
      {:ok, for(%{"result" => %{"resourceObjectId" => id, "permissionship" => "LOOKUP_PERMISSIONSHIP_HAS_PERMISSION"}} <- lines, do: "#{op.type}:#{id}")}
    end
  end

  @impl true
  def list_users(_state, _op), do: {:unsupported, "list_users"}

  defp post(state, path, body) do
    case Req.post(state.req, url: path, json: body) do
      {:ok, %{status: 200, body: b}} -> {:ok, JSON.decode!(b)}
      {:ok, %{status: s, body: b}} -> {:error, "#{s} #{msg(b)}"}
      {:error, e} -> {:error, Exception.message(e)}
    end
  end

  # a streaming call: one JSON object per line; an error line fails the call
  defp stream(state, path, body) do
    with {:ok, %{status: 200, body: b}} <- Req.post(state.req, url: path, json: body) do
      lines = for l <- String.split(b, "\n", trim: true), do: JSON.decode!(l)

      case Enum.find(lines, &Map.has_key?(&1, "error")) do
        nil -> {:ok, lines}
        %{"error" => e} -> {:error, msg(JSON.encode!(e))}
      end
    else
      {:ok, %{status: s, body: b}} -> {:error, "#{s} #{msg(b)}"}
      {:error, e} -> {:error, Exception.message(e)}
    end
  end

  defp msg(b) do
    case JSON.decode(b) do
      {:ok, %{"message" => m}} -> m
      _ -> String.slice(b, 0, 300)
    end
  end
end

defmodule Compare.Engines.Spicedb.Schema do
  @moduledoc """
  OpenFGA DSL (schema 1.1) to a SpiceDB schema, for the subset the scenarios use: directly
  assigned types (`user`, `user:*`, `group#member`), computed relations, `X from Y` (an arrow,
  `Y->X`), and `or`, `and`, `but not` (`+`, `&`, `-`). Conditions and modules are refused.

  A relation is a SpiceDB relation when it is only directly assigned types, a permission
  otherwise; one that mixes both becomes relation `<name>__direct` (the types) and permission
  `<name>`. -> {:ok, schema, renames} where renames maps {type, relation} to where its tuples go.
  """

  def from_fga(dsl) do
    lines =
      dsl
      |> String.split("\n")
      |> Enum.map(&String.trim/1)
      |> Enum.reject(&(&1 == "" or String.starts_with?(&1, "#")))

    if Enum.any?(lines, &String.starts_with?(&1, ["condition ", "module ", "extend type "])),
      do: {:error, "spicedb adapter: conditions and modules are not translated"},
      else: translate(lines)
  end

  defp translate(lines) do
    {types, _} =
      Enum.reduce(lines, {[], nil}, fn line, {types, current} ->
        cond do
          String.starts_with?(line, "type ") ->
            name = String.trim_leading(line, "type ")
            {types ++ [{name, []}], name}

          String.starts_with?(line, "define ") ->
            [name, expr] = line |> String.trim_leading("define ") |> String.split(":", parts: 2)
            {List.update_at(types, -1, fn {t, defs} -> {t, defs ++ [{String.trim(name), String.trim(expr)}]} end), current}

          true ->
            {types, current}
        end
      end)

    {defs, renames} =
      Enum.map_reduce(types, %{}, fn {type, defs}, renames ->
        {body, renames} = Enum.flat_map_reduce(defs, renames, &define(type, &1, &2))
        {"definition #{type} {\n" <> Enum.map_join(body, "", &"  #{&1}\n") <> "}\n", renames}
      end)

    {:ok, Enum.join(defs, "\n"), renames}
  rescue
    e in ArgumentError -> {:error, "spicedb adapter: " <> Exception.message(e)}
  end

  defp define(type, {name, expr}, renames) do
    {ast, []} = expr |> tokens() |> parse()

    case ast do
      {:direct, types} ->
        {["relation #{name}: #{Enum.join(types, " | ")}"], renames}

      ast ->
        direct = "#{name}__direct"

        case directs(ast) do
          [] -> {["permission #{name} = #{render(ast, direct)}"], renames}
          [types] -> {["relation #{direct}: #{Enum.join(types, " | ")}", "permission #{name} = #{render(ast, direct)}"], Map.put(renames, {type, name}, direct)}
          _ -> raise ArgumentError, "#{type}##{name}: more than one list of assignable types"
        end
    end
  end

  defp tokens(expr), do: Regex.scan(~r/\[[^\]]*\]|\(|\)|but not|[A-Za-z0-9_\-]+/, expr) |> List.flatten()

  # expr := term (op term)*, left-associative; OpenFGA requires parentheses to mix operators
  defp parse(tokens) do
    {left, rest} = term(tokens)
    more(left, rest)
  end

  defp more(left, [op | rest]) when op in ["or", "and", "but not"] do
    {right, rest} = term(rest)
    more({op, left, right}, rest)
  end

  defp more(left, rest), do: {left, rest}

  defp term(["(" | rest]) do
    {e, [")" | rest]} = parse(rest)
    {e, rest}
  end

  defp term(["[" <> _ = list | rest]) do
    types = list |> String.trim_leading("[") |> String.trim_trailing("]") |> String.split(",") |> Enum.map(&String.trim/1)
    if Enum.any?(types, &String.contains?(&1, " with ")), do: raise(ArgumentError, "conditions are not translated")
    {{:direct, types}, rest}
  end

  defp term([rel, "from", tupleset | rest]), do: {{:arrow, tupleset, rel}, rest}
  defp term([rel | rest]), do: {{:computed, rel}, rest}

  defp directs({:direct, types}), do: [types]
  defp directs({_op, l, r}), do: directs(l) ++ directs(r)
  defp directs(_), do: []

  @ops %{"or" => "+", "and" => "&", "but not" => "-"}

  defp render({:direct, _}, direct), do: direct
  defp render({:computed, r}, _), do: r
  defp render({:arrow, ts, r}, _), do: "#{ts}->#{r}"
  defp render({op, l, r}, d), do: "#{nested(l, d)} #{@ops[op]} #{nested(r, d)}"

  defp nested({op, _, _} = e, d) when is_binary(op), do: "(#{render(e, d)})"
  defp nested(e, d), do: render(e, d)
end
