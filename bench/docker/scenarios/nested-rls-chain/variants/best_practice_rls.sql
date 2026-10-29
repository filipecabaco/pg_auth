-- Flatten every level: one InitPlan array per table, built by SECURITY DEFINER helpers that skip the inner RLS.
alter table app.documents enable row level security;
alter table app.comments enable row level security;
alter table app.attachments enable row level security;
create policy d on app.documents for select to bench_user using (org_id = any ((select app.my_orgs())::int[]));
create policy c on app.comments for select to bench_user using (document_id = any ((select app.my_docs())::bigint[]));
create policy a on app.attachments for select to bench_user using (comment_id = any ((select app.my_comments())::bigint[]));
