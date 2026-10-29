defmodule Compare.Engine do
  @moduledoc """
  Contract every engine adapter implements. Drop a file `compare/lib/engines/<id>.exs` that defines
  `Compare.Engines.<Camelized id>` (`openfga` -> `Openfga`) with `@behaviour Compare.Engine` and the
  runner finds it by `--engines <id>`.

  Neutral data the adapter receives:

    * model   - OpenFGA DSL text (adapters translate if their engine speaks something else)
    * tuple   - %{object: "doc:1", relation: "viewer", user: "user:anne", condition: nil | %{name:, context:}}
    * op      - %{kind: :check | :list_objects | :list_users, ...}
        :check        subject, relation, object, contextual_tuples (list of tuples), context (map)
        :list_objects subject, relation, type, contextual_tuples, context
        :list_users   object, relation, filters (["user", "group#member"]), contextual_tuples, context

  Outcomes (return value of check/list_*):

    * {:ok, true | false}          for :check
    * {:ok, [string]}              for lists ("document:1", "user:anne", "user:*", "group:g#member")
    * {:error, message}            the engine refused the request (compared with "expects an error")
    * {:unsupported, reason}       the engine cannot answer this (reported, never a failure)

  Everything is called from arbitrary processes; keep `state` safe to share (pids, ports, urls).
  """

  @type state :: term
  @type tuple_ :: map
  @type op :: map
  @type outcome :: {:ok, term} | {:error, String.t()} | {:unsupported, String.t()}

  @doc "Start containers / connections. opts are the parsed CLI options."
  @callback setup(opts :: keyword) :: {:ok, state} | {:error, term}
  @callback teardown(state) :: :ok
  @callback name() :: String.t()
  @callback version(state) :: String.t()

  @doc """
  Features the engine supports. Operation kinds (:check, :list_objects, :list_users) and features
  (:contextual_tuples, :context, :conditions, :wildcards, :userset_subject). Anything a scenario
  requires that is not listed is reported as unsupported.
  """
  @callback capabilities(state) :: [atom]

  @doc "Throw away all models and tuples (fresh store/database). Returns the state to use next."
  @callback reset(state) :: {:ok, state}

  @doc "Replace the authorization model. {:error, msg} when the engine rejects it."
  @callback load_model(state, dsl :: String.t()) :: :ok | {:error, String.t()}

  @doc "Write tuples (they persist across load_model calls). Returns tuples the engine refused."
  @callback write_tuples(state, [tuple_]) :: {:ok, [%{tuple: tuple_, error: String.t()}]}

  @callback check(state, op) :: outcome
  @callback list_objects(state, op) :: outcome
  @callback list_users(state, op) :: outcome

  @doc "Optional: called once with every scenario before running (pre-translate models, warm caches)."
  @callback prepare(state, scenarios :: [map]) :: :ok

  # For pg_auth's sidecars (compare/lib/stack.exs: parity and the bench)
  @doc "Environment for a pg-auth-sidecar on the engine's network."
  @callback sidecar_env(state) :: [{String.t(), String.t()}]
  @doc "A new server on the same datastore (its caches empty). -> the new state."
  @callback fresh_server(state) :: state
  @callback delete_tuples(state, [tuple_]) :: :ok | {:error, String.t()}
  @doc "How long after a write the engine's default reads may still answer from before it."
  @callback staleness_ms(state) :: non_neg_integer
  @doc "The same for an answer the engine may have cached before the write."
  @callback revocation_ms(state) :: non_neg_integer
  @optional_callbacks prepare: 2, sidecar_env: 1, fresh_server: 1, delete_tuples: 2, staleness_ms: 1, revocation_ms: 1

  @doc "Resolve the adapter module for an id such as \"openfga\"."
  def module_for(id) do
    mod = Module.concat(Compare.Engines, Macro.camelize(id))

    if Code.ensure_loaded?(mod),
      do: mod,
      else: raise("no adapter for engine #{inspect(id)}: expected #{inspect(mod)} in compare/lib/engines/#{id}.exs")
  end

  @doc "Dispatch an op to the right callback, short-circuiting on missing capabilities."
  def run_op(mod, state, caps, op) do
    case missing(op, caps) do
      [] ->
        try do
          apply(mod, op.kind, [state, op])
        rescue
          e -> {:error, "adapter crashed: " <> Exception.message(e)}
        catch
          :exit, e -> {:error, "adapter exit: " <> inspect(e)}
        end

      miss ->
        {:unsupported, Enum.join(miss, "+")}
    end
  end

  def missing(op, caps) do
    required = [op.kind | Map.get(op, :requires, [])]
    Enum.reject(required, &(&1 in caps))
  end
end
