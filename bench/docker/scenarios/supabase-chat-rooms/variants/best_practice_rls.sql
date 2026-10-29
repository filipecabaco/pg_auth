-- Supabase performance advice: (select ...) InitPlans, the visible rooms as one array from a
-- security definer function, indexed columns, policies `to bench_user`, reactions keep a room_id.
alter table app.rooms enable row level security;
alter table app.room_members enable row level security;
alter table app.messages enable row level security;
alter table app.reactions enable row level security;
alter table app.room_reads enable row level security;
create policy p on app.rooms for select to bench_user using (id = any ((select app.visible_rooms())::int[]));
create policy p on app.room_members for select to bench_user using (room_id = any ((select app.visible_rooms())::int[]));
create policy p on app.messages for select to bench_user using (room_id = any ((select app.visible_rooms())::int[]));
create policy p on app.reactions for select to bench_user using (room_id = any ((select app.visible_rooms())::int[]));
create policy p on app.room_reads for select to bench_user using (user_id = (select auth.uid()));
