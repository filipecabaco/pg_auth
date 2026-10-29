-- @set: plan_cache_mode=force_custom_plan
-- InitPlan array policy (best plain RLS form). Role-agnostic (no TO clause).
-- Same policy with plan_cache_mode=force_custom_plan: re-plan on every execution (catalog's mitigation).
alter table app.documents enable row level security;
alter table app.documents_p enable row level security;
create policy p on app.documents for select using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
create policy p on app.documents_p for select using (org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid()))));
