defmodule Compare.Loader.Random do
  @moduledoc """
  Seeded random scenario: nested groups, a folder
  tree, wildcards, blocklists, `and` / `but not`. `scale` multiplies entity counts (1 = 200 users,
  30 groups, 50 folders, 500 documents). Same seed + scale + ops -> same scenario.

  Operations: `ops` checks (half uniform random, half drawn from subjects that actually reach the
  document), plus ops/10 list_objects and ops/20 list_users (at least 5 each).
  """

  @model """
  model
    schema 1.1

  type user

  type group
    relations
      define member: [user, group#member]

  type folder
    relations
      define parent: [folder]
      define viewer: [user, group#member] or viewer from parent

  type document
    relations
      define parent: [folder]
      define owner: [user]
      define blocked: [user]
      define viewer: [user, user:*, group#member] or owner or viewer from parent
      define can_view: viewer but not blocked
      define can_edit: owner and can_view
  """

  @perms ["viewer", "can_view", "can_edit"]

  def model, do: @model

  def load(opts) do
    seed = opts[:seed] || 1
    scale = opts[:scale] || 1
    n_ops = opts[:ops] || 500
    :rand.seed(:exsss, {seed, seed, seed})
    dim = %{users: 200 * scale, groups: 30 * scale, folders: 50 * scale, docs: 500 * scale}

    tuples = initial(dim)
    ops = ops(tuples, dim, n_ops)

    scn =
      Compare.Scenario.finalize(%{
        name: "random-seed#{seed}-x#{scale}",
        source: "random",
        stages: [%{model: @model, tuples: Enum.map(tuples, &to_map/1), ops: ops}],
        dataset: %{seed: seed, scale: scale, dim: dim, tuples: length(tuples)}
      })

    [scn]
  end

  defp to_map({o, r, u}), do: %{object: o, relation: r, user: u}

  # ---- data ----------------------------------------------------------------------------------

  defp user(d), do: "user:u#{:rand.uniform(d.users) - 1}"
  defp group(d), do: "group:g#{:rand.uniform(d.groups) - 1}"
  defp folder(d), do: "folder:f#{:rand.uniform(d.folders) - 1}"

  defp initial(d) do
    group_members =
      for g <- 0..(d.groups - 1), _ <- 1..Enum.random(2..6), do: {"group:g#{g}", "member", user(d)}

    nested =
      if d.groups < 2 do
        []
      else
        for g <- 0..(d.groups - 2), :rand.uniform() < 0.6 do
          {"group:g#{g}", "member", "group:g#{Enum.random((g + 1)..(d.groups - 1))}#member"}
        end
      end

    folder_parent = for f <- 1..(d.folders - 1), do: {"folder:f#{f}", "parent", "folder:f#{div(f - 1, 3)}"}
    folder_viewers = for _ <- 1..max(div(d.folders * 7, 10), 1), do: {folder(d), "viewer", viewer_subject(d)}
    docs = for i <- 0..(d.docs - 1), obj = "document:d#{i}", t <- doc_tuples(d, obj), do: t

    Enum.uniq(group_members ++ nested ++ folder_parent ++ folder_viewers ++ docs)
  end

  defp viewer_subject(d), do: if(:rand.uniform() < 0.5, do: user(d), else: group(d) <> "#member")

  defp doc_tuples(d, obj) do
    parent = if :rand.uniform() < 0.85, do: [{obj, "parent", folder(d)}], else: []
    owner = if :rand.uniform() < 0.7, do: [{obj, "owner", user(d)}], else: []
    viewers = for _ <- 1..Enum.random(0..3)//1, do: {obj, "viewer", viewer_subject(d)}
    public = if :rand.uniform() < 0.05, do: [{obj, "viewer", "user:*"}], else: []
    blocked = for _ <- 1..Enum.random(0..2)//1, do: {obj, "blocked", user(d)}
    parent ++ owner ++ viewers ++ public ++ blocked
  end

  # ---- operations ---------------------------------------------------------------------------

  defp ops(tuples, d, n) do
    by_obj = Enum.group_by(tuples, &elem(&1, 0))

    members =
      Enum.reduce(tuples, %{}, fn
        {g, "member", "user:" <> _ = u}, acc -> Map.update(acc, g, [u], &[u | &1])
        _, acc -> acc
      end)

    parent = for {o, "parent", f} <- tuples, into: %{}, do: {o, f}

    near = fn doc ->
      Stream.unfold(doc, fn
        nil -> nil
        o -> {o, parent[o]}
      end)
      |> Enum.flat_map(&Map.get(by_obj, &1, []))
      |> Enum.flat_map(fn {_, _, u} ->
        cond do
          String.starts_with?(u, "user:*") -> [user(d)]
          String.contains?(u, "#") -> Map.get(members, hd(String.split(u, "#")), [user(d)])
          true -> [u]
        end
      end)
    end

    doc = fn -> "document:d#{:rand.uniform(d.docs) - 1}" end

    checks =
      for i <- 1..n do
        dd = doc.()
        u = if rem(i, 2) == 0, do: user(d), else: Enum.random([user(d) | near.(dd)])
        %{kind: :check, subject: u, relation: Enum.random(@perms), object: dd}
      end

    list_objects =
      for _ <- 1..max(div(n, 10), 5) do
        %{kind: :list_objects, subject: user(d), relation: Enum.random(@perms), type: "document"}
      end

    list_users =
      for _ <- 1..max(div(n, 20), 5) do
        %{kind: :list_users, object: doc.(), relation: Enum.random(@perms), filters: ["user"]}
      end

    checks ++ list_objects ++ list_users
  end
end
