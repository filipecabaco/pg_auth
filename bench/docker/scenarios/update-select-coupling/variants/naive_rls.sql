-- Catalog (c): SELECT policy hides soft-deleted rows (NOT deleted AND ...), plus UPDATE/INSERT/DELETE.
-- Expect: soft_delete raises 42501 (the SELECT policy is also checked against the new row).
alter table app.documents enable row level security;
create policy s on app.documents for select to bench_user using (not deleted and org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy u on app.documents for update to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid())))) with check (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy i on app.documents for insert to bench_user with check (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy d on app.documents for delete to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
