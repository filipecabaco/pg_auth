-- Function-based policy (per-row plpgsql), on flat table and every partition path via the parent.
-- No "to <role>" clause: the policy must apply to bench_a and bench_b.
alter table app.documents enable row level security;
alter table app.documents_p enable row level security;
alter table app.memberships enable row level security;
create policy m on app.memberships for select using (user_id = (select app.uid()));
create policy p on app.documents for select using (app.is_member_plpgsql(org_id));
create policy p on app.documents_p for select using (app.is_member_plpgsql(org_id));
