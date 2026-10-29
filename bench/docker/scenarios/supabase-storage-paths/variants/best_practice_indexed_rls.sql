-- Same rule, written so an index can serve it: immutable split_part instead of the volatile
-- foldername(), matching the expression indexes in schema.sql. Not what the docs show.
alter table storage.objects enable row level security;
create policy p on storage.objects for select to bench_user using (
  bucket_id = 'public-assets'
  or (bucket_id = 'user-files' and split_part(name, '/', 1) = (select auth.uid()::text))
  or (bucket_id = 'team-files' and split_part(name, '/', 1)::int = any ((select app.my_teams())::int[])));
