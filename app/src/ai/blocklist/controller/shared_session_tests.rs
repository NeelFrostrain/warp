use super::reusable_bootstrap_conversation;
use crate::ai::agent::conversation::AIConversationId;

fn fixed_conversation_id() -> AIConversationId {
    AIConversationId::new()
}

/// The very first no-token bootstrap prompt for a task has nothing recorded yet, so it must
/// always fall through to starting a new conversation.
#[test]
fn no_recorded_conversation_means_no_reuse() {
    let reused = reusable_bootstrap_conversation(None, |_| {
        panic!("must not look up a conversation when nothing is recorded")
    });

    assert_eq!(reused, None);
}

/// A redelivered retry (warp-server resending the same bootstrap prompt after a lost ack) must
/// reuse the conversation created for the prior delivery while it has no server token yet -- the
/// core REMOTE-2661 fix.
#[test]
fn retry_reuses_recorded_conversation_without_a_token_yet() {
    let recorded = fixed_conversation_id();

    let reused = reusable_bootstrap_conversation(Some(recorded), |conversation_id| {
        assert_eq!(conversation_id, recorded);
        Some(false)
    });

    assert_eq!(
        reused,
        Some(recorded),
        "a retry must reuse the still-untokened conversation from a prior delivery"
    );
}

/// Once the recorded conversation has been assigned a server token, the bootstrap has durably
/// succeeded. A later no-token submission for the same task is a distinct request, not a retry of
/// this one, so it must not be forced onto the now-established conversation.
#[test]
fn tokened_conversation_is_not_reused() {
    let recorded = fixed_conversation_id();

    let reused = reusable_bootstrap_conversation(Some(recorded), |conversation_id| {
        assert_eq!(conversation_id, recorded);
        Some(true)
    });

    assert_eq!(
        reused, None,
        "a conversation that already has a server token must not be reused"
    );
}

/// If the recorded conversation has since been dropped (e.g. its terminal surface went away),
/// there's nothing to reuse; fall through to starting fresh rather than reusing a stale ID.
#[test]
fn vanished_conversation_is_not_reused() {
    let recorded = fixed_conversation_id();

    let reused = reusable_bootstrap_conversation(Some(recorded), |_| None);

    assert_eq!(
        reused, None,
        "a conversation that can no longer be found must not be reused"
    );
}
