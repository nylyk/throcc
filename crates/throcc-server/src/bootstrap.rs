use anyhow::Result;
use throcc_proto::Role;

use crate::{State, invite};

/// An Admin invite is minted whenever the allowlist is empty, since otherwise
/// there is no way in. The result is `None` once anyone is enrolled.
pub fn ensure_invite(state: &State) -> Result<Option<String>> {
    if state.database.user_count()? > 0 {
        return Ok(None);
    }

    let invite = state
        .database
        .replace_unredeemed_invites(Role::Admin, invite::DEFAULT_TTL)?;
    Ok(Some(invite.code))
}
