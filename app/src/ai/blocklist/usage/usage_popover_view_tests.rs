use std::collections::HashMap;

use warp_core::features::FeatureFlag;

use super::*;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::usage::rollup::{AgentAvatar, PerAgentCreditEntry};

fn model(id: &str, warp_tokens: u32, category: &str) -> ModelTokenUsage {
    ModelTokenUsage {
        model_id: id.to_string(),
        warp_tokens,
        warp_token_usage_by_category: HashMap::from([(category.to_string(), warp_tokens)]),
        ..Default::default()
    }
}

#[test]
fn model_usage_rows_drops_zero_token_models() {
    let models = vec![
        model("gpt-5.5", 100, PRIMARY_AGENT_CATEGORY),
        ModelTokenUsage {
            model_id: "unused-model".to_string(),
            ..Default::default()
        },
    ];
    let rows = model_usage_rows(&models, &HashMap::new());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model_id, "gpt-5.5");
}

#[test]
fn model_usage_rows_sorts_primary_agent_first() {
    let models = vec![
        model("codex-model", 50, FULL_TERMINAL_USE_CATEGORY),
        model("primary-model", 100, PRIMARY_AGENT_CATEGORY),
        model("auto-model", 10, "other_category"),
    ];
    let rows = model_usage_rows(&models, &HashMap::new());
    assert_eq!(rows[0].model_id, "primary-model");
    assert_eq!(rows[0].role_badge, Some("Primary agent"));
}

#[test]
fn model_usage_rows_assigns_full_terminal_use_badge() {
    let models = vec![model("codex-model", 50, FULL_TERMINAL_USE_CATEGORY)];
    let rows = model_usage_rows(&models, &HashMap::new());
    assert_eq!(rows[0].role_badge, Some("Full terminal use"));
}

#[test]
fn model_usage_rows_has_no_badge_for_unknown_categories() {
    let models = vec![model("auto-model", 10, "some_other_category")];
    let rows = model_usage_rows(&models, &HashMap::new());
    assert_eq!(rows[0].role_badge, None);
}

#[test]
fn model_usage_rows_joins_cost_by_model_id() {
    let models = vec![
        model("gpt-5.5", 100, PRIMARY_AGENT_CATEGORY),
        model("codex-model", 50, FULL_TERMINAL_USE_CATEGORY),
    ];
    let costs = HashMap::from([("gpt-5.5".to_string(), 36.0)]);
    let rows = model_usage_rows(&models, &costs);
    let gpt_row = rows.iter().find(|r| r.model_id == "gpt-5.5").unwrap();
    let codex_row = rows.iter().find(|r| r.model_id == "codex-model").unwrap();
    assert_eq!(gpt_row.cost_in_cents, Some(36.0));
    assert_eq!(codex_row.cost_in_cents, None);
}

fn per_agent_entry(name: &str, credits: f32) -> PerAgentCreditEntry {
    PerAgentCreditEntry {
        conversation_id: AIConversationId::new(),
        display_name: name.to_string(),
        avatar: AgentAvatar::Child,
        credits_spent: credits,
        cost_in_cents: None,
        tokens: None,
    }
}

#[test]
fn truncate_rollup_rows_shows_all_under_cap() {
    let entries: Vec<_> = (0..3)
        .map(|i| per_agent_entry(&format!("agent-{i}"), 1.0))
        .collect();
    let (shown, hidden) = truncate_rollup_rows(&entries, false);
    assert_eq!(shown.len(), 3);
    assert_eq!(hidden, 0);
}

#[test]
fn truncate_rollup_rows_truncates_over_cap_until_show_all() {
    let entries: Vec<_> = (0..8)
        .map(|i| per_agent_entry(&format!("agent-{i}"), 1.0))
        .collect();
    let (shown, hidden) = truncate_rollup_rows(&entries, false);
    assert_eq!(shown.len(), ROLLUP_TRUNCATION_CAP);
    assert_eq!(hidden, 3);

    let (shown_all, hidden_all) = truncate_rollup_rows(&entries, true);
    assert_eq!(shown_all.len(), 8);
    assert_eq!(hidden_all, 0);
}

#[test]
fn format_token_count_abbreviates_above_1000() {
    assert_eq!(format_token_count(500), "500");
    assert_eq!(format_token_count(9600), "9.6k");
    assert_eq!(format_token_count(1000), "1.0k");
}

#[test]
fn format_tokens_and_cost_omits_dollar_suffix_when_flag_disabled() {
    let _flag = FeatureFlag::PricingTransparency.override_enabled(false);

    assert_eq!(
        format_tokens_and_cost(Some(9600), Some(36.0)),
        "9.6k tokens"
    );
}

#[test]
fn format_tokens_and_cost_joins_tokens_and_dollar_with_a_slash_when_flag_enabled() {
    let _flag = FeatureFlag::PricingTransparency.override_enabled(true);

    assert_eq!(
        format_tokens_and_cost(Some(9600), Some(36.0)),
        "9.6k tokens / $0.36"
    );
}

#[test]
fn format_tokens_and_cost_omits_dollar_suffix_when_cost_is_unknown() {
    let _flag = FeatureFlag::PricingTransparency.override_enabled(true);

    assert_eq!(format_tokens_and_cost(Some(9600), None), "9.6k tokens");
}

#[test]
fn format_tokens_and_cost_falls_back_to_cost_only_when_tokens_are_unknown() {
    let _flag = FeatureFlag::PricingTransparency.override_enabled(true);

    assert_eq!(format_tokens_and_cost(None, Some(36.0)), "$0.36");
}

#[test]
fn format_tokens_and_cost_shows_em_dash_when_both_are_unknown() {
    let _flag = FeatureFlag::PricingTransparency.override_enabled(true);

    assert_eq!(format_tokens_and_cost(None, None), "\u{2014}");
}
