-- Memberships and roles the RLS variants read from app.room_members, public rooms as a wildcard,
-- and each user as the only one who can see their own read state.
select 'room:' || room_id, case role when 'moderator' then 'moderator' else 'member' end, 'user:' || user_id
  from app.room_members
union all
select 'room:' || id, 'public', 'user:*' from app.rooms where is_public
union all
select distinct 'user:' || user_id, 'owner', 'user:' || user_id from app.room_reads
