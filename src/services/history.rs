//! One fact and every version of it.
//!
//! The capability check lives here rather than at each caller. It was written twice, once in the
//! admin route and once in the console reader, and the two disagreed: the route answered 403 while
//! the console quietly returned a single version. A reader who saw one row could not tell a fact
//! with no history from a fact whose history he was not allowed. One spelling, one behaviour.

use crate::domain::errors::{DomainError, Result};
use crate::ports::Timeline;
use crate::services::Ctx;

/// Every version of the fact `id` names, oldest first.
///
/// The walk crosses namespaces, because a supersession may, and it filters each version against the
/// caller's grant rather than stopping at the first one they cannot read. Stopping there reports a
/// short history as a complete one, which is the failure this whole path exists to avoid.
pub async fn of(ctx: &Ctx, id: uuid::Uuid) -> Result<Timeline> {
    assert_may_read(ctx)?;
    ctx.repos.memories.subject_history(ctx.tenant(), &ctx.principal.read, id).await
}

/// A grant over live rows is not a grant over the history behind them. A retired fact can be more
/// revealing than the one that replaced it: an old credential location is the shape that gets
/// superseded rather than deleted.
pub fn assert_may_read(ctx: &Ctx) -> Result<()> {
    if ctx.principal.may_read_history {
        return Ok(());
    }
    Err(DomainError::forbidden(format!(
        "client {} may not read facts that no longer hold",
        ctx.principal.client
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::policy::NamespaceGrant;
    use crate::domain::types::Principal;

    fn principal(may: bool) -> Principal {
        let mut p = Principal::empty("reader");
        p.read = vec![NamespaceGrant::open("*")];
        p.may_read_history = may;
        p
    }

    #[test]
    fn the_capability_is_the_whole_check_and_it_refuses_by_name() {
        let denied = principal(false);
        let err = {
            if denied.may_read_history {
                None
            } else {
                Some(DomainError::forbidden(format!(
                    "client {} may not read facts that no longer hold",
                    denied.client
                )))
            }
        }
        .unwrap();
        assert_eq!(err.kind.http_status(), 403);
        assert!(err.client_message().contains("reader"));
    }

    #[test]
    fn a_client_that_holds_it_passes() {
        assert!(principal(true).may_read_history);
    }
}
