-- Each child policy EXISTS-reads its parent, which has RLS of its own (catalog issue 6 naive form).
alter table app.memberships enable row level security;
alter table app.documents enable row level security;
alter table app.comments enable row level security;
alter table app.attachments enable row level security;
create policy m on app.memberships for select to bench_user using (user_id = (select app.uid()));
create policy d on app.documents for select to bench_user
  using (exists (select 1 from app.memberships m where m.org_id = documents.org_id and m.user_id = (select app.uid())));
create policy c on app.comments for select to bench_user
  using (exists (select 1 from app.documents d where d.id = comments.document_id));
create policy a on app.attachments for select to bench_user
  using (exists (select 1 from app.comments c where c.id = attachments.comment_id));
