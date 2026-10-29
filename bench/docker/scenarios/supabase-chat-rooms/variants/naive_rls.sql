-- Slack-clone / docs style: a per-row security definer helper everywhere, unwrapped auth.uid(),
-- reactions gated through the messages they point at (nested RLS).
alter table app.rooms enable row level security;
alter table app.room_members enable row level security;
alter table app.messages enable row level security;
alter table app.reactions enable row level security;
alter table app.room_reads enable row level security;
create policy p on app.rooms for select to bench_user using (app.can_see_room(id));
create policy p on app.room_members for select to bench_user using (app.can_see_room(room_id));
create policy p on app.messages for select to bench_user using (app.can_see_room(room_id));
create policy p on app.reactions for select to bench_user
  using (exists (select 1 from app.messages m where m.id = message_id));
create policy p on app.room_reads for select to bench_user using (user_id = auth.uid());
