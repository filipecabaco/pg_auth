# Agent skills for pg_auth

Three skills for coding agents (Claude Code, and any agent that reads `SKILL.md` skills) working
on an app that uses pg_auth:

| skill | for |
|---|---|
| [`pg-auth-mapping`](pg-auth-mapping/SKILL.md) | writing or fixing the YAML mapping between your tables and an engine's rules, with a reference per engine (OpenFGA, SpiceDB, Cerbos, Cedar) and every key |
| [`pg-auth-migrations`](pg-auth-migrations/SKILL.md) | the migrations that store and change rules and mappings: order, re-runs, updates, the first admin, dry runs, checks after deploy |
| [`pg-auth-debug`](pg-auth-debug/SKILL.md) | a setup that misbehaves: no rows, refused writes, failing queries, grants that do not show, slow queries |

Install them into a project:

```sh
cp -r skills/pg-auth-* .claude/skills/       # from a checkout you have read
npx skills add filipecabaco/pg_auth          # or fetch them: the default branch, as it is now
```

Skills are instructions your agent follows with your permissions: read them before installing,
as you would a script. `npx skills add` fetches whatever the repository's default branch holds
when you run it.

They follow the docs ([Getting started](../docs/usage.md)); change both together.
