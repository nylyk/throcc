use throcc_proto::ids::Role;

pub const DAILY_INVITES: u32 = 3;

pub fn can_manage_rooms(actor: Role) -> bool {
    actor >= Role::Manager
}

pub fn can_list_users(actor: Role) -> bool {
    actor == Role::Admin
}

pub fn can_edit_server(actor: Role) -> bool {
    actor == Role::Admin
}

/// Admins mint without a cap; everyone else gets [`DAILY_INVITES`] per rolling day.
pub fn invite_cap(actor: Role) -> Option<u32> {
    (actor != Role::Admin).then_some(DAILY_INVITES)
}

/// Only admins change roles, only on strictly lower ranks, and only to a strictly lower rank.
pub fn can_set_role(actor: Role, target: Role, new: Role) -> bool {
    actor == Role::Admin && target < actor && new < actor
}

/// Only admins remove users, and only users of strictly lower rank.
pub fn can_remove(actor: Role, target: Role) -> bool {
    actor == Role::Admin && target < actor
}

#[cfg(test)]
mod tests {
    use super::*;
    use Role::*;

    #[test]
    fn ranks_are_strict() {
        assert!(!can_remove(Manager, Admin));
        assert!(!can_remove(Manager, User));
        assert!(!can_remove(Admin, Admin));
        assert!(can_remove(Admin, Manager));
        assert!(can_set_role(Admin, User, Manager));
        assert!(!can_set_role(Admin, User, Admin));
        assert!(!can_set_role(Manager, User, User));
        assert!(can_manage_rooms(Manager) && !can_manage_rooms(User));
        assert_eq!(invite_cap(User), Some(3));
        assert_eq!(invite_cap(Manager), Some(3));
        assert_eq!(invite_cap(Admin), None);
    }
}
