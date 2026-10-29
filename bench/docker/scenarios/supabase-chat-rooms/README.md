# supabase-chat-rooms

The Slack-clone shape every Supabase chat tutorial ends up with: `rooms`, `room_members` (with a
role), `messages`, `reactions`, plus per-user read state (`room_reads`). A row is visible when the
room is public or the user is a member (member or moderator) of the room.

## The pattern

- `messages`, `reactions` (with a denormalized `room_id`), `room_members` and `rooms` are all gated
  by the same room-level question. `room_reads` is per user (`user_id = auth.uid()`).
- Subjects: `two_rooms` (member of rooms 1 and 2, plus the public rooms), `many_rooms` (member of 40%
  of all rooms), `moderator` (moderator of public room 3, which holds 5% of all messages).
- Queries (all use `auth.uid()` like an app would): `latest50` (a room's page),
  `unread` (join `room_reads` x `messages`, group by room), `search` (`body like`, unindexed),
  `members` (member list of the big room), `my_rooms_last` (user's rooms with last message via
  LATERAL), `reactions` (group by emoji for a room), `count`.

## Why RLS is good / bad here

- Good: a room-scoped page (`latest50`, `reactions`, `members`) names one `room_id`, the policy is
  an `= any(array)` InitPlan and the `(room_id, created_at desc, id desc)` index serves it; best
  practice RLS is about as fast as the explicit filter.
- Bad: the policy cannot read `room_members` from `room_members` (42P17), so it goes through a
  security definer helper. Per row (`naive_rls`) that is one function call per scanned message:
  `search` and `count` are 100x+ slower. The array fix scales with the user's room count
  (`many_rooms`) and public rooms. `naive_rls` gates reactions through `messages` (nested RLS).

## What pg_auth does

One `room` object (`viewer: member or moderator or public`, public as a `user:*` wildcard tuple)
protects `rooms.id`, `room_members.room_id`, `messages.room_id`, `reactions.room_id`.
`room_reads` uses a second pattern: an object per user with an `owner` relation to herself
(`protect('app.room_reads','user','user_id','owner')`), i.e. "my own rows" expressed as an object.
Tuples are at membership level (members, moderators, public rooms, one per user for read state).
Limitation: `room_reads` as a per-user object works, but needs one tuple per user.

## Indexes and what each query should use

- `latest50`: `messages(room_id, created_at desc, id desc)`, index scan, no sort.
- `unread`: `room_reads(user_id, room_id)` pk, then per room the same messages index (range on
  `created_at`), group by room.
- `search`: no index (seq scan of visible rooms); shows the cost of the policy filter alone.
- `members`: `room_members(room_id, role)` / pk. `my_rooms_last`: `room_members(user_id, room_id)`
  then the messages index, one row per room (LATERAL, LIMIT 1).
- `reactions`: `reactions(room_id, emoji)`, index-only. Policy columns: `room_members(user_id, room_id)`,
  partial `rooms(id) where is_public`.

## Scale

`:scale` = messages; rooms = scale/50, users = scale/20, reactions = scale. 10x = 1M messages, 20k
rooms, 50k users. Everything is `generate_series` inserts; OpenFGA tuples = memberships (~2 per user)
+ public rooms + one per user with reads.

## Sources

- Supabase Slack-clone example: https://github.com/supabase/supabase/tree/master/examples/slack-clone
  (rooms/channels/messages/members + `authorize` helper style policies)
- RLS performance advice (`(select auth.uid())`, security definer helpers, indexes, `to`):
  https://supabase.com/docs/guides/database/postgres/row-level-security (sections "Add indexes" and "Call functions with select")
- Postgres recursion limitation for policies on their own table (42P17):
  https://www.postgresql.org/docs/current/sql-createpolicy.html

## Results (scale 100000, 3 reps, 2 CPU / 2 GB, median ms, shared host so indicative)

| subject / query | baseline | naive_rls | best_practice_rls | pg_auth | pg_auth_per_row |
|---|---|---|---|---|---|
| two_rooms / search | 1.2 | 150 | 0.93 | 1.1 | 3.9 |
| two_rooms / count | 0.63 | 342 | 0.33 | 0.56 | 6.5 |
| many_rooms / unread | 2.8 | 64 | 6.1 | 6.2 | 5.0 |
| many_rooms / my_rooms_last | 1.9 | 13 | 7.3 | 34.9 | 2.0 |
| moderator / latest50 | 0.006 | 0.012 | 0.018 | 0.025 | 0.038 |

pg_auth matches best-practice RLS and is 100x+ better than naive. It loses on `many_rooms /
my_rooms_last` (listed-ids plan with ~800 rooms inside a LATERAL) while the per-row sidecar is fast there.
All variants returned identical results.
