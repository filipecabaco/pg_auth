-- Catalog fix: never mark a function that logs / raises on its input LEAKPROOF.
alter function app.spy(int) not leakproof;
alter function app.spy_i(int) not leakproof;
alter function app.to_int(text) not leakproof;
alter table app.docs enable row level security;
create policy p on app.docs for select to bench_user
  using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
