-- The policy people write on a partitioned table: array subquery on the parent (issue 15 'naive').
-- Same shape as best_practice here: plain RLS cannot do better than the InitPlan array.
alter table app.documents_p16 enable row level security;
alter table app.documents_p128 enable row level security;
create policy p on app.documents_p16 for select to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy p on app.documents_p128 for select to bench_user using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
