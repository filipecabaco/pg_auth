-- Per-row PL/pgSQL function in the policy; nested RLS on memberships and comments.
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
alter table app.documents_p enable row level security;
alter table app.comments enable row level security;
create policy p on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy p on app.documents for select to bench_user using (app.is_member_plpgsql(org_id));
create policy p on app.documents_p for select to bench_user using (app.is_member_plpgsql(org_id));
create policy p on app.comments for select to bench_user
  using (exists (select 1 from app.documents d where d.id = document_id));
