defmodule Compare.Stack do
  @moduledoc """
  An engine (OpenFGA by default, or SpiceDB) and pg-auth-sidecars on one Docker network, for a
  Postgres container with pg_auth to join (`pg_docker_args/1`). The sidecars share their sockets with Postgres through a volume
  mounted at /run/pg_auth; each sidecar has its own LIST_LIMIT, so a session picks the
  enforcement path by choosing a socket (`pg_auth.sidecar_socket = socket(name)`).

  Used by compare/parity.exs and bench/docker/run.exs. Everything started is tracked, so `stop/1`
  removes it even when startup fails halfway.
  """
  alias Compare.Docker
  alias Compare.Engines.{Openfga, Spicedb}

  @sockets "/run/pg_auth"

  # PG_AUTH_IMAGE_TAG picks other builds of the images (scripts/coverage.exs: instrumented ones)
  def pg_image, do: tagged("pg_auth-bench")
  def sidecar_image, do: tagged("pg_auth-sidecar")
  defp tagged(image), do: if(tag = System.get_env("PG_AUTH_IMAGE_TAG"), do: "#{image}:#{tag}", else: image)
  def socket(name), do: "#{@sockets}/#{name}.sock"

  def build(root) do
    for {image, args} <- [
          {sidecar_image(), ["-t", sidecar_image(), "sidecar/query"]},
          {pg_image(), ["-f", "bench/docker/Dockerfile", "-t", pg_image(), "."]}
        ] do
      IO.puts(:stderr, "building #{image}")
      {_, 0} = System.cmd("docker", ["build" | args], cd: root, into: IO.stream(:stderr, :line))
    end
  end

  @doc "Network, volume and a registry of started containers. `label` is the docker label (k=v) on all of them."
  def new(prefix, label) do
    # the OS pid keeps names apart between runs (unique_integer restarts with every VM)
    id = "#{prefix}-#{System.pid()}-#{System.unique_integer([:positive])}"
    docker!(["network", "create", "--label", label, id])
    docker!(["volume", "create", "--label", label, id])
    {:ok, started} = Agent.start(fn -> [] end)
    %{id: id, net: id, vol: id, label: label, started: started}
  end

  @doc """
  Start the engine with `model` (OpenFGA DSL) loaded, then one sidecar per `{name, list_limit}`
  in `sidecars`. Returns the stack with :engine (adapter state) and :engine_mod. Options:

    * `:engine` - "openfga" (default), "spicedb", "cerbos" or "cedar" (in the sidecar: give the
      sidecars PG_AUTH_DATABASE_URL), each set up as in production
    * `:policies` - Cerbos's policy directory
    * `:list_max_results` - OpenFGA's listObjectsMaxResults (default 1_000_000, so a sidecar's
      LIST_LIMIT alone decides when a subject has too many objects; production default is 1000).
      SpiceDB keeps its own limit (1000 per LookupResources call), which the sidecar pages past.
    * `:sidecar_cache_ms` - the sidecars' CACHE_TTL_MS (default 0: every statement reaches the
      engine, as parity needs; nil keeps the sidecar's production default of 1000)
    * `:sidecar_env` - extra environment for every sidecar

  OpenFGA's ListObjects deadline is raised to 30 s, so parity's ListObjects reference is complete
  (at the deadline OpenFGA answers with a partial list); the sidecars are told the same deadline.
  """
  def start_engine(stack, model, sidecars, opts \\ []) do
    {mod, engine, env} = setup(Keyword.get(opts, :engine, "openfga"), stack, opts)
    for c <- [engine.db, engine.container], c, do: track(stack, c)
    {:ok, engine} = mod.reset(engine)
    :ok = mod.load_model(engine, model)

    stack = Map.merge(stack, %{engine: engine, engine_mod: mod, engine_env: env, sidecar_opts: opts})
    start_sidecars(stack, sidecars)
    stack
  end

  defp setup("openfga", stack, opts) do
    max = Keyword.get(opts, :list_max_results, 1_000_000)

    {:ok, fga} =
      Openfga.setup(
        [network: stack.net,
           docker_args: ["-e", "OPENFGA_LIST_OBJECTS_MAX_RESULTS=#{max}", "-e", "OPENFGA_LIST_OBJECTS_DEADLINE=30s",
                         "-e", "OPENFGA_REQUEST_TIMEOUT=60s"]]
      )

    # the sidecars know the deadline raised here
    {Openfga, fga, [{"OPENFGA_LIST_DEADLINE_MS", "30000"}]}
  end

  defp setup("spicedb", stack, _opts) do
    {:ok, spicedb} = Spicedb.setup(network: stack.net)
    {Spicedb, spicedb, []}
  end

  defp setup("cedar", _stack, _opts) do
    {:ok, cedar} = Compare.Engines.Cedar.setup([])
    {Compare.Engines.Cedar, cedar, []}
  end

  defp setup("cerbos", stack, opts) do
    {:ok, cerbos} = Compare.Engines.Cerbos.setup(network: stack.net, policies: opts[:policies], server_args: opts[:cerbos_args])
    {Compare.Engines.Cerbos, cerbos, []}
  end

  @doc """
  Start one sidecar per `{name, list_limit}` in `sidecars`, each a new container, with the
  options given to start_engine plus `env`. -> their container names.
  """
  def start_sidecars(stack, sidecars, env \\ []) do
    opts = stack.sidecar_opts

    engine =
      Enum.flat_map(stack.engine_mod.sidecar_env(stack.engine) ++ stack.engine_env, fn {k, v} -> ["-e", "#{k}=#{v}"] end) ++
        case Keyword.get(opts, :sidecar_cache_ms, 0) do
          nil -> []
          ms -> ["-e", "CACHE_TTL_MS=#{ms}"]
        end ++
        Enum.flat_map(Keyword.get(opts, :sidecar_env, []) ++ env, fn {k, v} -> ["-e", "#{k}=#{v}"] end)

    # one at a time: the first mount initialises the volume with the image's socket directory
    for {name, limit} <- sidecars do
      container = track(stack, "#{stack.id}-sidecar-#{name}-#{System.unique_integer([:positive])}")
      docker!(["run", "-d", "--label", stack.label, "--name", container | pg_docker_args(stack)] ++
                engine ++ Docker.coverage_args("sidecar") ++ ["-e", "PG_AUTH_SOCKET=#{socket(name)}", "-e", "LIST_LIMIT=#{limit}", sidecar_image()])
      wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", container, "test", "-S", socket(name)])) end, container)
      container
    end
  end

  @doc "Replace the engine's server with a new container on the same datastore (its caches empty)."
  def fresh_engine(stack) do
    engine = stack.engine_mod.fresh_server(stack.engine)
    # Cedar runs in the sidecars: no server of its own
    if engine.container, do: track(stack, engine.container)
    %{stack | engine: engine}
  end

  @doc "Remove these containers (one variant's sidecars, say)."
  def remove(names), do: for(n <- names, do: Docker.stop(n))

  @doc """
  Write tuples (%{object:, relation:, user:}) to the stack's engine, and wait until its default
  reads see them (SpiceDB answers from a revision up to its quantization window old).
  """
  def write_tuples(stack, tuples) do
    tuples = Enum.map(tuples, &Map.put_new(&1, :condition, nil))

    case stack.engine_mod.write_tuples(stack.engine, tuples) do
      {:ok, []} -> Process.sleep(stack.engine_mod.staleness_ms(stack.engine))
      {:ok, refused} -> raise "#{stack.engine_mod.name()} refused #{length(refused)} tuples, e.g. #{inspect(hd(refused))}"
    end
  end

  @doc "`docker run` arguments that put a container on the stack's network with the sockets mounted."
  def pg_docker_args(stack), do: ["--network", stack.net, "-v", "#{stack.vol}:#{@sockets}"]

  def track(stack, name) do
    Agent.update(stack.started, &[name | &1])
    name
  end

  def stop(stack) do
    for c <- containers(stack), do: Docker.stop(c)
    System.cmd("docker", ["volume", "rm", "-f", stack.vol], stderr_to_stdout: true)
    System.cmd("docker", ["network", "rm", stack.net], stderr_to_stdout: true)
  end

  def logs(stack), do: for(c <- containers(stack), into: %{}, do: {c, Docker.logs(c)})

  defp containers(stack), do: Agent.get(stack.started, & &1)

  def wait(ready?, name, tries \\ 120) do
    cond do
      ready?.() -> :ok
      tries > 0 -> Process.sleep(500) && wait(ready?, name, tries - 1)
      true -> raise "#{name} never became ready:\n#{Docker.logs(name)}"
    end
  end

  defp docker!(args) do
    case System.cmd("docker", args, stderr_to_stdout: true) do
      {out, 0} -> String.trim(out)
      {out, s} -> raise "docker #{Enum.join(args, " ")} failed (#{s}): #{out}"
    end
  end
end
