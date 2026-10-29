-- Vulnerable pattern: membership table has own-row RLS, but the helper (owner rights) answers for
-- any user id, and the documents policy calls it per row.
alter table app.memberships enable row level security;
create policy m on app.memberships for select to bench_user using (user_id = (select app.uid()));
alter table app.docs enable row level security;
create policy d on app.docs for select to bench_user using (app.is_member_of(app.uid(), org_id));
