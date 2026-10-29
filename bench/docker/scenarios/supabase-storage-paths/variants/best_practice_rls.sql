-- Supabase's advice applied to the documented policy: (select auth.uid()), teams as a security
-- definer array, `to bench_user`. storage.foldername is still the volatile plpgsql function.
alter table storage.objects enable row level security;
create policy p on storage.objects for select to bench_user using (
  bucket_id = 'public-assets'
  or (bucket_id = 'user-files' and (storage.foldername(name))[1] = (select auth.uid()::text))
  or (bucket_id = 'team-files' and (storage.foldername(name))[1]::int = any ((select app.my_teams())::int[])));
