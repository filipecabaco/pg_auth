-- The Supabase Storage docs policies, copy-pasted: volatile storage.foldername(name), unwrapped
-- auth.uid(), team folder resolved with a per-row EXISTS on a (row-secured) membership table.
alter table storage.objects enable row level security;
alter table app.team_members enable row level security;
create policy p on app.team_members for select to bench_user using (user_id = auth.uid());
create policy p on storage.objects for select to bench_user using (
  bucket_id = 'public-assets'
  or (bucket_id = 'user-files' and (storage.foldername(name))[1] = auth.uid()::text)
  or (bucket_id = 'team-files' and exists (
        select 1 from app.team_members tm
        where tm.team_id = (storage.foldername(name))[1]::int and tm.user_id = auth.uid())));
