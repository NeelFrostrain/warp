use super::*;

#[test]
fn color_for_model_is_deterministic() {
    assert_eq!(color_for_model("gpt-5.5"), color_for_model("gpt-5.5"));
}

#[test]
fn color_for_model_differs_across_distinct_models_in_practice() {
    // Not a strict guarantee (hash collisions are possible with only 6
    // buckets), but with these particular sample ids we expect at least
    // one pair to differ, guarding against an accidental constant return.
    let colors: Vec<_> = ["gpt-5.5", "gpt-5.3-codex", "auto", "kimi-k2.6"]
        .iter()
        .map(|id| color_for_model(id))
        .collect();
    assert!(colors.iter().any(|c| *c != colors[0]));
}
