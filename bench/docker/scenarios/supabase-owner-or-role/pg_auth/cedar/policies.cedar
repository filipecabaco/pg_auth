// Cedar resolves the principal's side when it plans: an admin's plan is every row, anyone else's
// owner_id = their id, which the owner_id index serves
permit (principal, action == Action::"read", resource is Todo)
  when { resource.owner_id == principal || (principal.app_metadata has role && ["admin", "support"].contains(principal.app_metadata.role)) };
