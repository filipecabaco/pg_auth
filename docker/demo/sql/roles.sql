-- Who connects. As on Supabase: each person's requests carry their token's claims in
-- request.jwt.claims (PostgREST sets it per request; here it is set on each login role), and
-- they act as the role `authenticated`.
create role authenticated nologin;
create role admin login superuser;
create role anne login in role authenticated;
create role bob login in role authenticated;
alter role anne set request.jwt.claims = '{"sub": "anne", "role": "authenticated"}';
alter role bob set request.jwt.claims = '{"sub": "bob", "role": "authenticated"}';
