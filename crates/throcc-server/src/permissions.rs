use throcc_proto::{ErrorCode, Response, Role};

pub fn require_rank_above(actor: Role, target: Role) -> Result<(), Response> {
    if actor.rank() > target.rank() {
        return Ok(());
    }
    Err(Response::Err {
        code: ErrorCode::Denied,
        message: format!("{actor:?} cannot act on {target:?}"),
    })
}

pub fn require_role(actor: Role, needed: Role) -> Result<(), Response> {
    if actor.rank() >= needed.rank() {
        return Ok(());
    }
    Err(Response::Err {
        code: ErrorCode::Denied,
        message: format!("{needed:?} is required, {actor:?} is not enough"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_cannot_act_on_its_own_rank() {
        assert!(require_rank_above(Role::Admin, Role::Admin).is_err());
        assert!(require_rank_above(Role::Admin, Role::Manager).is_ok());
        assert!(require_rank_above(Role::Manager, Role::Admin).is_err());
        assert!(require_rank_above(Role::Manager, Role::User).is_ok());
        assert!(require_rank_above(Role::User, Role::User).is_err());
    }

    #[test]
    fn a_capability_needs_its_own_rank_or_higher() {
        assert!(require_role(Role::Admin, Role::Admin).is_ok());
        assert!(require_role(Role::Admin, Role::Manager).is_ok());
        assert!(require_role(Role::Manager, Role::Admin).is_err());
    }
}
