-- Catalog (a)/(b): only FOR UPDATE and FOR INSERT policies (+ DELETE), no FOR SELECT policy.
-- Expect: UPDATE/DELETE/upsert affect 0 rows silently; INSERT ... RETURNING raises 42501.
alter table app.documents enable row level security;
create policy u on app.documents for update to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid())))) with check (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy i on app.documents for insert to bench_user with check (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy d on app.documents for delete to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
