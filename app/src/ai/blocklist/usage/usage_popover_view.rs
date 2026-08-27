//! The "Conversation" usage popover (pricing-transparency Surfaces 1, 2,
//! 6): a click-triggered floating popover anchored to the footer's usage
//! icon (Surface 1) that replaces the collapsed-inline usage footer with a
//! richer breakdown: a per-model stacked bar with role pill badges and
//! per-model dollar costs (Surface 2), and — when the conversation is an
//! orchestrator with locally-loaded descendants — a per-agent stacked-bar
//! rollup in place of the per-model section (Surface 6).
//!
//! The context-window breakdown (originally Surface 4) has moved to its own
//! separately-triggered surface and is not rendered here.
//!
//! Unlike [`super::conversation_usage_view::ConversationUsageView`] (which
//! renders inline in the block list and remains the production
//! implementation of the older per-block "1B" pill), this view is a
//! self-contained floating popover with its own section-collapse state.
//! Per the pricing-transparency specs' resolved decisions, "1A" (this view,
//! triggered from the footer icon) is the new canonical entry point; 1B is
//! not removed here but is expected to be deprecated in a follow-up once 1A
//! ships.
//!
//! All derived data (credits, token/model breakdown, context-window
//! segments, response timing, orchestration rollup) is recomputed from the
//! live [`AIConversation`] on every render rather than snapshotted at
//! construction, so the popover's numbers update live while streaming
//! (matching the existing per-block pill's live-update behavior).

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use thousands::Separable;
use warp_core::ui::Icon;
use warp_core::ui::theme::WarpTheme;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss,
    DispatchEventResult, DropShadow, Empty, EventHandler, Expanded, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Radius, Shrinkable, Stack, Text,
};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::ai::blocklist::agent_view::orchestration_pill_bar::{
    render_agent_avatar_disc, render_orchestrator_avatar_disc,
};
use crate::ai::blocklist::usage::colors::color_for_model;
use crate::ai::blocklist::usage::rollup::{
    AgentAvatar, OrchestrationCreditRollup, PerAgentCreditEntry, compute_orchestration_rollup,
};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::persistence::model::{
    FULL_TERMINAL_USE_CATEGORY, ModelTokenUsage, PRIMARY_AGENT_CATEGORY, PersistedModelTokenCost,
};
use crate::settings_view::SettingsSection;
use crate::ui_components::blended_colors;
use crate::workspace::WorkspaceAction;

/// Fixed popover width, matching the Figma reference (`336px`).
const POPOVER_WIDTH: f32 = 336.;
/// Maximum number of per-agent rollup rows shown before truncating behind
/// "Show N more" (PRODUCT invariant carried over from the pre-existing
/// rollup feature).
const ROLLUP_TRUNCATION_CAP: usize = 5;
/// Height of the segmented usage/context-window bars.
const BAR_HEIGHT: f32 = 6.;
/// Width/height of the small color swatch next to each row label.
const SWATCH_SIZE: f32 = 8.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsagePopoverAction {
    ToggleModelUsageSection,
    ToggleToolCallSummarySection,
    ToggleResponseTimeSection,
    ShowAllRollupAgents,
    ShowFewerRollupAgents,
    /// Dispatched by the [`Dismiss`] underlay when the user clicks outside
    /// the popover.
    RequestClose,
    /// Toggles the per-model token/cost breakdown subsection for the given
    /// model id.
    ToggleModelExpanded(String),
}

/// Emitted when the popover should be closed, so the footer (which owns
/// `usage_popover_open`) can react to an outside click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsagePopoverEvent {
    Close,
}

/// A `Flex::row` preconfigured for `label ... value` rows: cross-axis
/// centered, and `SpaceBetween` + `MainAxisSize::Max` so the two ends
/// actually push apart (an easy warpui footgun: `SpaceBetween` alone has no
/// effect unless the row is also told to claim the max available width).
fn space_between_row() -> Flex {
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
}

/// Floating "Conversation" usage popover. Holds only section-expand UI
/// state; all usage data is read live from [`BlocklistAIHistoryModel`] at
/// render time. The footer owns a single long-lived instance and calls
/// [`Self::reset_for_conversation`] each time the popover opens (see the
/// footer wiring), so section-collapse state always resets to its default
/// on reopen per the spec's resolved decisions, without ever constructing a
/// new view mid-click-dispatch.
pub struct UsagePopoverView {
    conversation_id: AIConversationId,
    model_usage_section_expanded: bool,
    tool_call_summary_section_expanded: bool,
    response_time_section_expanded: bool,
    rollup_show_all: bool,
    /// Model ids whose per-model breakdown subsection is currently expanded.
    /// Keyed by model id rather than a fixed set of fields since the list of
    /// models is dynamic per-conversation.
    expanded_model_ids: HashSet<String>,
    /// Persistent per-tooltip hover state, keyed by a descriptive string
    /// unique to each hoverable instance (e.g. a model id for the "full
    /// model name" tooltip, or `"value:model:{id}"` for that model's
    /// token-count tooltip). Lazily populated since the set of rows is
    /// dynamic; wrapped in a `RefCell` so it can be populated from
    /// `View::render`'s `&self`. A fresh `MouseStateHandle::default()` on
    /// every render would never register as hovered, since the hover-in
    /// delay needs a stable handle across renders to fire.
    hover_states: RefCell<HashMap<String, MouseStateHandle>>,
    model_usage_toggle_mouse_state: MouseStateHandle,
    tool_call_summary_toggle_mouse_state: MouseStateHandle,
    response_time_toggle_mouse_state: MouseStateHandle,
    show_more_mouse_state: MouseStateHandle,
    show_fewer_mouse_state: MouseStateHandle,
    view_account_usage_mouse_state: MouseStateHandle,
}

impl UsagePopoverView {
    pub fn new(conversation_id: AIConversationId) -> Self {
        Self {
            conversation_id,
            // All sections default to expanded, matching the "rev" Figma
            // proposals (Surface 2 spec §5).
            model_usage_section_expanded: true,
            tool_call_summary_section_expanded: true,
            response_time_section_expanded: true,
            rollup_show_all: false,
            expanded_model_ids: HashSet::new(),
            hover_states: RefCell::new(HashMap::new()),
            model_usage_toggle_mouse_state: MouseStateHandle::default(),
            tool_call_summary_toggle_mouse_state: MouseStateHandle::default(),
            response_time_toggle_mouse_state: MouseStateHandle::default(),
            show_more_mouse_state: MouseStateHandle::default(),
            show_fewer_mouse_state: MouseStateHandle::default(),
            view_account_usage_mouse_state: MouseStateHandle::default(),
        }
    }

    /// Points this (reused) popover at `conversation_id` and resets all
    /// section-collapse/rollup-truncation state back to its default,
    /// exactly matching what [`Self::new`] would produce. Called by the
    /// footer each time the popover is opened, so reopening always starts
    /// from a clean slate without allocating a new view.
    ///
    /// Notifies the view context so the popover is actually re-rendered:
    /// `ViewContext::update` does not implicitly mark a view dirty, so
    /// without this the popover kept painting its stale initial render
    /// (constructed with a placeholder conversation id that never matches,
    /// so it rendered empty) even after being pointed at a real
    /// conversation.
    pub fn reset_for_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ViewContext<Self>,
    ) {
        *self = Self::new(conversation_id);
        ctx.notify();
    }

    fn render_header(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let title = Text::new(
            "Conversation".to_string(),
            appearance.ui_font_family(),
            appearance.ui_font_size() + 4.,
        )
        .with_color(blended_colors::text_main(theme, background))
        .finish();

        let link_color = blended_colors::text_sub(theme, background);
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let link = Hoverable::new(self.view_account_usage_mouse_state.clone(), move |_state| {
            Text::new("View account usage".to_string(), font_family, font_size)
                .with_color(link_color)
                .with_selectable(false)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::ShowSettingsPage(
                SettingsSection::BillingAndUsage,
            ));
        })
        .finish();

        space_between_row()
            .with_child(title)
            .with_child(link)
            .finish()
    }

    /// Renders a collapsible section header: an overline `label` on the
    /// left and, on the right, a chevron indicating expand state. When the
    /// section is collapsed and `collapsed_summary` is provided, that
    /// summary text (e.g. "144.3k tokens / $0.21", "12 tool calls") is
    /// shown just before the chevron, so key information stays visible
    /// without expanding the section.
    #[allow(clippy::too_many_arguments)]
    fn render_section_header(
        &self,
        label: &str,
        expanded: bool,
        collapsed_summary: Option<String>,
        collapsed_summary_tooltip: Option<String>,
        mouse_state: MouseStateHandle,
        action: UsagePopoverAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let label_color = blended_colors::text_disabled(theme, background);
        let summary_color = blended_colors::text_sub(theme, background);
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        // Fetched up front (rather than inside the closure below) so the
        // closure never needs to capture `self`.
        let summary_hover_state = collapsed_summary_tooltip
            .is_some()
            .then(|| self.hover_state_for(format!("value:header:{label}")));
        let label = label.to_string();
        let overline_font_family = appearance.overline_font_family();
        // A couple points larger than the raw overline size so the section
        // headers read more clearly against the row content below them.
        let overline_font_size = appearance.overline_font_size() + 2.;
        let summary_font_family = appearance.ui_font_family();
        let summary_font_size = appearance.ui_font_size();

        Hoverable::new(mouse_state, move |_state| {
            let label_element = Text::new(label.clone(), overline_font_family, overline_font_size)
                .with_color(label_color)
                .finish();
            let icon_element =
                ConstrainedBox::new(icon.to_warpui_icon(label_color.into()).finish())
                    .with_width(overline_font_size)
                    .with_height(overline_font_size)
                    .finish();
            let mut right = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.);
            if !expanded && let Some(summary) = &collapsed_summary {
                let summary_element =
                    Text::new(summary.clone(), summary_font_family, summary_font_size)
                        .with_color(summary_color)
                        .finish();
                let summary_element = match (&summary_hover_state, &collapsed_summary_tooltip) {
                    (Some(hover_state), Some(tooltip_text)) => with_tooltip(
                        hover_state.clone(),
                        summary_element,
                        tooltip_text.clone(),
                        appearance,
                    ),
                    _ => summary_element,
                };
                right.add_child(summary_element);
            }
            right.add_child(icon_element);
            space_between_row()
                .with_child(label_element)
                .with_child(right.finish())
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
    }

    /// Renders a non-collapsible section header with a value on the right
    /// (same visual treatment as a collapsible header's collapsed-state
    /// summary, minus the chevron): an overline `label` on the left, `value`
    /// on the right, no click handling. Used for sections (e.g. Platform
    /// Usage) that have no expand/collapse state or separate content rows.
    fn render_static_section_header_with_value(
        &self,
        label: &str,
        value: String,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let label_color = blended_colors::text_disabled(theme, background);
        let value_color = blended_colors::text_sub(theme, background);

        space_between_row()
            .with_child(
                Text::new(
                    label.to_string(),
                    appearance.overline_font_family(),
                    appearance.overline_font_size() + 2.,
                )
                .with_color(label_color)
                .finish(),
            )
            .with_child(
                Text::new(
                    value,
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(value_color)
                .finish(),
            )
            .finish()
    }

    /// Renders either the per-model breakdown (default) or, when an
    /// orchestration rollup applies, the per-agent breakdown in its place
    /// (Surface 6 resolved decision 2). The section header's collapsed
    /// summary carries the conversation- (or rollup-) wide total usage
    /// figure, replacing what used to be a standalone "Total Usage" row
    /// above this section.
    fn render_usage_breakdown_section(
        &self,
        conversation: &AIConversation,
        rollup: Option<&OrchestrationCreditRollup>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let (total_tokens, total_cost_in_cents) = total_usage_tokens_and_cost(conversation, rollup);
        let collapsed_summary = format_tokens_and_cost(total_tokens, total_cost_in_cents);
        let collapsed_summary_tooltip = total_tokens.and_then(exact_token_count_tooltip);

        let mut column = Flex::column().with_spacing(8.);
        if let Some(rollup) = rollup {
            column.add_child(self.render_section_header(
                "AGENT USAGE",
                self.model_usage_section_expanded,
                Some(collapsed_summary),
                collapsed_summary_tooltip,
                self.model_usage_toggle_mouse_state.clone(),
                UsagePopoverAction::ToggleModelUsageSection,
                appearance,
            ));
            if self.model_usage_section_expanded {
                column.add_child(self.render_agent_rollup_rows(rollup, appearance));
            }
        } else {
            column.add_child(self.render_section_header(
                "INFERENCE USAGE",
                self.model_usage_section_expanded,
                Some(collapsed_summary),
                collapsed_summary_tooltip,
                self.model_usage_toggle_mouse_state.clone(),
                UsagePopoverAction::ToggleModelUsageSection,
                appearance,
            ));
            if self.model_usage_section_expanded {
                column.add_child(self.render_model_usage_rows(conversation, appearance));
            }
        }
        column.finish()
    }

    /// Renders the non-collapsible "PLATFORM USAGE" section: Warp's
    /// platform fee (infrastructure/orchestration overhead), which unlike
    /// inference cost isn't attributable to any single model, so it's
    /// shown as a single label/value row (no separate content, no
    /// expand/collapse) rather than folded into the inference usage
    /// breakdown.
    fn render_platform_usage_section(
        &self,
        conversation: &AIConversation,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let platform_cost_in_cents = conversation
            .usage_totals()
            .charged_usage
            .map(|charged_usage| charged_usage.platform_cost_in_cents);

        // Hidden entirely (rather than shown as "$0.00") when there's no
        // platform fee to report, e.g. conversations predating the
        // server-side platform fee, or ones where it happens to be zero.
        if platform_cost_in_cents.is_none_or(|cost| cost <= 0.) {
            return Empty::new().finish();
        }

        self.render_static_section_header_with_value(
            "PLATFORM USAGE",
            format_cost_only(platform_cost_in_cents),
            appearance,
        )
    }

    fn render_model_usage_rows(
        &self,
        conversation: &AIConversation,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        // Token counts (with role-category info for the badges) come from a
        // different underlying structure than the per-model charged-usage
        // breakdown (cost + input/output/cache/web split), so join them here
        // by model id.
        let rows = model_usage_rows(
            conversation.token_usage(),
            conversation.charged_usage_by_model(),
        );
        if rows.is_empty() {
            return Empty::new().finish();
        }
        let total_tokens: u64 = rows.iter().map(|r| r.tokens).sum();
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();

        let mut column = Flex::column().with_spacing(6.);

        // "All models" summary row. Uses the conversation-wide cost total
        // (the same source as the header's "Total Usage" row) rather than
        // re-summing per-model costs, so the two always agree even if a
        // model's cost hasn't been individually attributed yet.
        let all_models_value = Text::new(
            format_tokens_and_cost(
                Some(total_tokens),
                conversation.usage_totals().cost_in_cents,
            ),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_main(theme, background))
        .finish();
        let all_models_value = self.maybe_with_tooltip(
            "value:all_models".to_string(),
            all_models_value,
            exact_token_count_tooltip(total_tokens),
            appearance,
        );
        column.add_child(
            space_between_row()
                .with_child(
                    Text::new(
                        "All models".to_string(),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_sub(theme, background))
                    .finish(),
                )
                .with_child(all_models_value)
                .finish(),
        );

        // Stacked bar: one segment per model, proportional to token share.
        let segments: Vec<(ColorU, f32)> = rows
            .iter()
            .map(|row| {
                let pct = if total_tokens == 0 {
                    0.
                } else {
                    (row.tokens as f32 / total_tokens as f32) * 100.
                };
                (color_for_model(&row.model_id), pct)
            })
            .collect();
        column.add_child(render_segmented_bar(
            &segments,
            theme.outline().into_solid(),
        ));

        for row in &rows {
            column.add_child(self.render_model_usage_row(row, appearance));
        }

        column.finish()
    }

    /// Renders a per-model row. Every row is clickable (a trailing chevron
    /// indicates this) and toggles a breakdown subsection beneath it — an
    /// input/output/cache/web-search split when the model has a known
    /// charged-usage breakdown, or a fallback message when it doesn't (e.g.
    /// the server hasn't sent per-model charges for this conversation yet).
    fn render_model_usage_row(
        &self,
        row: &ModelUsageRow,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();
        let color = color_for_model(&row.model_id);
        let expanded = self.expanded_model_ids.contains(&row.model_id);

        let full_label = match row.role_badge {
            Some(role) => format!("{} ({role})", row.model_id),
            None => row.model_id.clone(),
        };
        let chevron_color = blended_colors::text_disabled(theme, background);
        let chevron_icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };

        // Model name and role badge are separate `Text`s (rather than one
        // formatted string) so they can use different colors. The name uses
        // `Shrinkable` (not `Expanded`) so it only claims as much width as it
        // actually needs (ellipsis-clipping once it runs out of room)
        // instead of being force-stretched to fill the row — `Expanded`
        // would leave the role badge stranded at the far right of the row
        // instead of immediately following a short model name.
        let mut label_row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        label_row.add_child(
            Shrinkable::new(
                1.,
                Text::new(row.model_id.clone(), appearance.ui_font_family(), font_size)
                    .with_color(blended_colors::text_main(theme, background))
                    .soft_wrap(false)
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
            )
            .finish(),
        );
        if let Some(role) = row.role_badge {
            label_row.add_child(
                Text::new(format!(" ({role})"), appearance.ui_font_family(), font_size)
                    .with_color(blended_colors::text_sub(theme, background))
                    .finish(),
            );
        }

        let label_with_tooltip = with_tooltip(
            self.hover_state_for(format!("label:model:{}", row.model_id)),
            label_row.finish(),
            full_label,
            appearance,
        );

        // The label is wrapped in `Expanded`: a plain (non-flex) `Text` in a
        // `Flex::row` sizes to its own intrinsic width regardless of the
        // row's available space, so a long model name would push the
        // trailing token/cost value and chevron off the edge of the popover
        // instead of being ellipsis-clipped. `Expanded` bounds it to
        // whatever space remains after its row-sibling (the swatch here).
        let left = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(7.)
            .with_child(render_swatch(color))
            .with_child(Expanded::new(1., label_with_tooltip).finish());

        let value = Text::new(
            format_tokens_and_cost(Some(row.tokens), row.cost_in_cents),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_main(theme, background))
        .finish();
        let value = self.maybe_with_tooltip(
            format!("value:model:{}", row.model_id),
            value,
            exact_token_count_tooltip(row.tokens),
            appearance,
        );
        let chevron =
            ConstrainedBox::new(chevron_icon.to_warpui_icon(chevron_color.into()).finish())
                .with_width(10.)
                .with_height(10.)
                .finish();
        let right = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(value)
            .with_child(chevron);

        let summary_row = space_between_row()
            .with_child(Expanded::new(1., left.finish()).finish())
            .with_child(Container::new(right.finish()).with_margin_left(8.).finish())
            .finish();

        let mut column = Flex::column().with_spacing(6.).with_child(summary_row);
        if expanded {
            let breakdown = match row.charged_usage {
                Some(charged_usage) => {
                    self.render_charged_usage_breakdown(&row.model_id, &charged_usage, appearance)
                }
                None => Text::new(
                    "No detailed breakdown available".to_string(),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(blended_colors::text_disabled(theme, background))
                .finish(),
            };
            // Extra right padding pulls the breakdown's trailing token/cost
            // values in from the popover's edge, so they visibly underhang
            // (rather than overhang) the model total row's own value, which
            // stops short of the edge to make room for its chevron.
            column.add_child(
                Container::new(breakdown)
                    .with_padding_left(15.)
                    .with_padding_right(20.)
                    .finish(),
            );
        }

        let model_id = row.model_id.clone();
        EventHandler::new(column.finish())
            .on_left_mouse_down(move |ctx, _, _| {
                ctx.dispatch_typed_action(UsagePopoverAction::ToggleModelExpanded(
                    model_id.clone(),
                ));
                DispatchEventResult::StopPropagation
            })
            .finish()
    }

    /// Renders a model's input/output/cache/web-search charged-usage
    /// breakdown, shown beneath a per-model row when expanded. Rows are
    /// omitted for categories the model didn't incur (e.g. cache tokens
    /// only apply to Anthropic models, and web searches are relatively
    /// rare). Each token-count row gets an "exact amount" tooltip when its
    /// value is large enough to be abbreviated, keyed by `model_id` plus a
    /// per-category suffix so each row has its own persistent hover state.
    fn render_charged_usage_breakdown(
        &self,
        model_id: &str,
        charged_usage: &PersistedModelTokenCost,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_spacing(4.);
        if charged_usage.total_input > 0 {
            column.add_child(self.render_label_value_row_with_tooltip(
                format!("value:model:{model_id}:input"),
                "Input tokens",
                format_tokens_and_cost(
                    Some(charged_usage.total_input),
                    Some(charged_usage.input_cost_in_cents),
                ),
                exact_token_count_tooltip(charged_usage.total_input),
                appearance,
            ));
        }
        if charged_usage.output > 0 {
            column.add_child(self.render_label_value_row_with_tooltip(
                format!("value:model:{model_id}:output"),
                "Output tokens",
                format_tokens_and_cost(
                    Some(charged_usage.output),
                    Some(charged_usage.output_cost_in_cents),
                ),
                exact_token_count_tooltip(charged_usage.output),
                appearance,
            ));
        }
        if charged_usage.input_cache_read > 0 {
            column.add_child(self.render_label_value_row_with_tooltip(
                format!("value:model:{model_id}:cache_read"),
                "Cache read tokens",
                format_tokens_and_cost(
                    Some(charged_usage.input_cache_read),
                    Some(charged_usage.input_cache_read_cost_in_cents),
                ),
                exact_token_count_tooltip(charged_usage.input_cache_read),
                appearance,
            ));
        }
        if charged_usage.input_cache_write > 0 {
            column.add_child(self.render_label_value_row_with_tooltip(
                format!("value:model:{model_id}:cache_write"),
                "Cache write tokens",
                format_tokens_and_cost(
                    Some(charged_usage.input_cache_write),
                    Some(charged_usage.input_cache_write_cost_in_cents),
                ),
                exact_token_count_tooltip(charged_usage.input_cache_write),
                appearance,
            ));
        }
        if charged_usage.web_search_count > 0 {
            column.add_child(render_label_value_row(
                "Web searches",
                format_count_and_cost(
                    charged_usage.web_search_count as u32,
                    "searches",
                    charged_usage.web_search_cost_in_cents,
                ),
                appearance,
            ));
        }
        column.finish()
    }

    /// Like [`render_label_value_row`], but wraps the value in a hover
    /// tooltip (see [`Self::maybe_with_tooltip`]) when `tooltip_text` is
    /// present -- used for token-count rows whose displayed value may be
    /// abbreviated.
    fn render_label_value_row_with_tooltip(
        &self,
        key: String,
        label: &str,
        value: String,
        tooltip_text: Option<String>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();
        let value_element = Text::new(value, appearance.ui_font_family(), font_size)
            .with_color(blended_colors::text_main(theme, background))
            .finish();
        let value_element = self.maybe_with_tooltip(key, value_element, tooltip_text, appearance);
        space_between_row()
            .with_child(
                Text::new(label.to_string(), appearance.ui_font_family(), font_size)
                    .with_color(blended_colors::text_sub(theme, background))
                    .finish(),
            )
            .with_child(value_element)
            .finish()
    }

    /// Fetches (lazily creating if needed) the persistent hover state
    /// backing a tooltip keyed by `key`. See `hover_states`' docs for why
    /// persistence (vs. a fresh `MouseStateHandle::default()` per render)
    /// matters.
    fn hover_state_for(&self, key: impl Into<String>) -> MouseStateHandle {
        self.hover_states
            .borrow_mut()
            .entry(key.into())
            .or_default()
            .clone()
    }

    /// Wraps `content` in a hover tooltip showing `tooltip_text`, when
    /// present -- e.g. the exact token count behind an abbreviated "9.6k
    /// tokens" figure (see [`exact_token_count_tooltip`]). Returns
    /// `content` unchanged when `tooltip_text` is `None` (nothing to
    /// disambiguate, so no tooltip is worth showing).
    fn maybe_with_tooltip(
        &self,
        key: String,
        content: Box<dyn Element>,
        tooltip_text: Option<String>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        match tooltip_text {
            Some(tooltip_text) => {
                with_tooltip(self.hover_state_for(key), content, tooltip_text, appearance)
            }
            None => content,
        }
    }

    /// Per-agent breakdown (Surface 6), adopting the same stacked-bar +
    /// swatch treatment as the per-model breakdown rather than Surface 6's
    /// original plain label/value list.
    fn render_agent_rollup_rows(
        &self,
        rollup: &OrchestrationCreditRollup,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();

        let mut column = Flex::column().with_spacing(6.);
        let all_agents_tokens = rollup.total_tokens.map(u64::from);
        let all_agents_value = Text::new(
            format_tokens_and_cost(all_agents_tokens, rollup.total_cost_in_cents),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_main(theme, background))
        .finish();
        let all_agents_value = self.maybe_with_tooltip(
            "value:all_agents".to_string(),
            all_agents_value,
            all_agents_tokens.and_then(exact_token_count_tooltip),
            appearance,
        );
        column.add_child(
            space_between_row()
                .with_child(
                    Text::new(
                        "All agents".to_string(),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_sub(theme, background))
                    .finish(),
                )
                .with_child(all_agents_value)
                .finish(),
        );

        let segments: Vec<(ColorU, f32)> = rollup
            .per_agent
            .iter()
            .map(|entry| {
                let pct = if rollup.total_credits <= 0. {
                    0.
                } else {
                    (entry.credits_spent / rollup.total_credits) * 100.
                };
                (agent_row_color(entry, theme), pct)
            })
            .collect();
        column.add_child(render_segmented_bar(
            &segments,
            theme.outline().into_solid(),
        ));

        let (shown, hidden_count) = truncate_rollup_rows(&rollup.per_agent, self.rollup_show_all);
        for entry in shown {
            column.add_child(self.render_agent_rollup_row(entry, appearance));
        }
        if hidden_count > 0 {
            column.add_child(self.render_show_more_link(hidden_count, appearance));
        } else if self.rollup_show_all && rollup.per_agent.len() > ROLLUP_TRUNCATION_CAP {
            column.add_child(self.render_show_fewer_link(appearance));
        }

        column.finish()
    }

    fn render_agent_rollup_row(
        &self,
        entry: &PerAgentCreditEntry,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();
        const ROW_AVATAR_SIZE: f32 = 16.;
        let avatar = match entry.avatar {
            AgentAvatar::Orchestrator => {
                render_orchestrator_avatar_disc(ROW_AVATAR_SIZE, theme, appearance)
            }
            AgentAvatar::Child => {
                render_agent_avatar_disc(&entry.display_name, ROW_AVATAR_SIZE, theme, appearance)
            }
        };
        let name = Text::new(
            entry.display_name.clone(),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_main(theme, background))
        .soft_wrap(false)
        .with_clip(ClipConfig::ellipsis())
        .finish();
        let entry_tokens = entry.tokens.map(u64::from);
        let value = Text::new(
            format_tokens_and_cost(entry_tokens, entry.cost_in_cents),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_sub(theme, background))
        .finish();
        let value = self.maybe_with_tooltip(
            format!("value:agent:{}", entry.conversation_id),
            value,
            entry_tokens.and_then(exact_token_count_tooltip),
            appearance,
        );

        space_between_row()
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_spacing(8.)
                    .with_child(avatar)
                    .with_child(name)
                    .finish(),
            )
            .with_child(value)
            .finish()
    }

    fn render_show_more_link(
        &self,
        hidden_count: usize,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        render_text_link(
            format!("Show {hidden_count} more"),
            self.show_more_mouse_state.clone(),
            UsagePopoverAction::ShowAllRollupAgents,
            appearance,
        )
    }

    /// "Show fewer" affordance (Surface 6 resolved decision 4): once "Show
    /// N more" has been clicked, this provides a way back to the truncated
    /// view without collapsing and reopening the whole section.
    fn render_show_fewer_link(&self, appearance: &Appearance) -> Box<dyn Element> {
        render_text_link(
            "Show fewer".to_string(),
            self.show_fewer_mouse_state.clone(),
            UsagePopoverAction::ShowFewerRollupAgents,
            appearance,
        )
    }

    fn render_tool_call_summary_section(
        &self,
        conversation: &AIConversation,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let tool_usage = conversation.tool_usage_metadata();
        let mut column = Flex::column().with_spacing(8.);
        column.add_child(self.render_section_header(
            "TOOL CALL SUMMARY",
            self.tool_call_summary_section_expanded,
            Some(format!("{} tool calls", tool_usage.total_tool_calls())),
            None,
            self.tool_call_summary_toggle_mouse_state.clone(),
            UsagePopoverAction::ToggleToolCallSummarySection,
            appearance,
        ));
        if !self.tool_call_summary_section_expanded {
            return column.finish();
        }

        let mut inner = Flex::column().with_spacing(4.);
        inner.add_child(render_label_value_row(
            "Tool calls",
            format!("{}", tool_usage.total_tool_calls()),
            appearance,
        ));
        inner.add_child(render_label_value_row(
            "Files changed",
            format!("{}", tool_usage.apply_file_diff_stats.files_changed),
            appearance,
        ));
        inner.add_child(render_diffs_row(
            tool_usage.apply_file_diff_stats.lines_added,
            tool_usage.apply_file_diff_stats.lines_removed,
            appearance,
        ));
        inner.add_child(render_label_value_row(
            "Commands executed",
            format!("{}", tool_usage.run_command_stats.commands_executed),
            appearance,
        ));
        column.add_child(inner.finish());
        column.finish()
    }

    fn render_response_time_section(
        &self,
        conversation: &AIConversation,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let ttft_ms = conversation.time_to_first_token_for_last_user_query_ms();
        let response_ms = conversation.total_agent_response_time_since_last_user_query_ms();
        let wall_ms = conversation.wall_to_wall_response_time_since_last_query();
        if ttft_ms == 0 && response_ms == 0 && wall_ms.unwrap_or(0) == 0 {
            return Empty::new().finish();
        }

        // Prefer the wall-to-wall total (including tool call time) for the
        // collapsed summary, since that's the most representative single
        // "total time" figure; fall back to agent response time alone when
        // the wall-clock total isn't available.
        let total_time_ms = wall_ms.filter(|&ms| ms != 0).unwrap_or(response_ms);

        let mut column = Flex::column().with_spacing(8.);
        column.add_child(self.render_section_header(
            "RESPONSE TIME",
            self.response_time_section_expanded,
            Some(format!("{:.1}s", total_time_ms as f64 / 1000.)),
            None,
            self.response_time_toggle_mouse_state.clone(),
            UsagePopoverAction::ToggleResponseTimeSection,
            appearance,
        ));
        if !self.response_time_section_expanded {
            return column.finish();
        }

        let mut inner = Flex::column().with_spacing(4.);
        inner.add_child(render_label_value_row(
            "Time to first token",
            format!("{:.1} seconds", ttft_ms as f64 / 1000.),
            appearance,
        ));
        inner.add_child(render_label_value_row(
            "Total agent response time",
            format!("{:.1} seconds", response_ms as f64 / 1000.),
            appearance,
        ));
        if let Some(wall_ms) = wall_ms
            && wall_ms != 0
        {
            inner.add_child(render_label_value_row(
                "Total time (including tool calls)",
                format!("{:.1} seconds", wall_ms as f64 / 1000.),
                appearance,
            ));
        }
        column.add_child(inner.finish());
        column.finish()
    }
}

impl View for UsagePopoverView {
    fn ui_name() -> &'static str {
        "UsagePopoverView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let history = BlocklistAIHistoryModel::as_ref(app);
        let Some(conversation) = history.conversation(&self.conversation_id) else {
            return Empty::new().finish();
        };
        let rollup = compute_orchestration_rollup(self.conversation_id, history);

        let mut column = Flex::column().with_spacing(12.);
        column.add_child(self.render_header(appearance));
        column.add_child(self.render_usage_breakdown_section(
            conversation,
            rollup.as_ref(),
            appearance,
        ));
        column.add_child(self.render_platform_usage_section(conversation, appearance));
        column.add_child(self.render_tool_call_summary_section(conversation, appearance));
        column.add_child(self.render_response_time_section(conversation, appearance));

        let content = Container::new(column.finish())
            .with_background(theme.surface_2())
            .with_border(Border::all(1.).with_border_color(theme.outline().into_solid()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .with_uniform_padding(12.)
            .finish();

        let popover = ConstrainedBox::new(content)
            .with_width(POPOVER_WIDTH)
            .finish();

        // Swallows any left-click that lands within the popover's own bounds
        // (even on inert content like labels/padding) before it ever reaches
        // `Dismiss`'s outside-click check below — otherwise every click inside
        // the popover that doesn't land on an interactive element (a link,
        // section header, etc.) would be treated as an "outside" click and
        // close the popover.
        let popover = EventHandler::new(popover)
            .on_left_mouse_down(|_, _, _| DispatchEventResult::StopPropagation)
            .finish();

        Dismiss::new(popover)
            .prevent_interaction_with_other_elements()
            .on_dismiss(|ctx, _app| {
                ctx.dispatch_typed_action(UsagePopoverAction::RequestClose);
            })
            .finish()
    }
}

impl Entity for UsagePopoverView {
    type Event = UsagePopoverEvent;
}

impl TypedActionView for UsagePopoverView {
    type Action = UsagePopoverAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            UsagePopoverAction::ToggleModelUsageSection => {
                self.model_usage_section_expanded = !self.model_usage_section_expanded;
                ctx.notify();
            }
            UsagePopoverAction::ToggleToolCallSummarySection => {
                self.tool_call_summary_section_expanded = !self.tool_call_summary_section_expanded;
                ctx.notify();
            }
            UsagePopoverAction::ToggleResponseTimeSection => {
                self.response_time_section_expanded = !self.response_time_section_expanded;
                ctx.notify();
            }
            UsagePopoverAction::ShowAllRollupAgents => {
                self.rollup_show_all = true;
                ctx.notify();
            }
            UsagePopoverAction::ShowFewerRollupAgents => {
                self.rollup_show_all = false;
                ctx.notify();
            }
            UsagePopoverAction::RequestClose => {
                ctx.emit(UsagePopoverEvent::Close);
            }
            UsagePopoverAction::ToggleModelExpanded(model_id) => {
                if !self.expanded_model_ids.remove(model_id) {
                    self.expanded_model_ids.insert(model_id.clone());
                }
                ctx.notify();
            }
        }
    }
}

/// One row of the per-model usage breakdown, aggregated across a model's
/// warp/byok/custom-endpoint token counts. Role badges mirror the existing
/// credits-based breakdown's category constants. `charged_usage` comes from
/// a separate per-model structure (`AIConversation::charged_usage_by_model`)
/// joined in by model id, since the token/category source
/// (`AIConversation::token_usage`) doesn't carry cost; `cost_in_cents` is
/// derived from it for the collapsed row's value text.
struct ModelUsageRow {
    model_id: String,
    role_badge: Option<&'static str>,
    tokens: u64,
    cost_in_cents: Option<f32>,
    charged_usage: Option<PersistedModelTokenCost>,
}

/// Builds the sorted per-model row list from raw per-conversation token
/// usage. Rows are ordered primary-agent-first (matching the existing
/// credits-based breakdown's sort), then alphabetically by model id.
fn model_usage_rows(
    models: &[ModelTokenUsage],
    charged_usage_by_model: &HashMap<String, PersistedModelTokenCost>,
) -> Vec<ModelUsageRow> {
    let mut rows: Vec<ModelUsageRow> = models
        .iter()
        .filter_map(|model| {
            let tokens = model.warp_tokens as u64
                + model.byok_tokens as u64
                + model.custom_endpoint_tokens as u64;
            if tokens == 0 {
                return None;
            }
            let role_badge = role_badge_for_model(model);
            let charged_usage = charged_usage_by_model.get(&model.model_id).copied();
            Some(ModelUsageRow {
                model_id: model.model_id.clone(),
                role_badge,
                tokens,
                cost_in_cents: charged_usage.map(|usage| usage.cost_in_cents()),
                charged_usage,
            })
        })
        .collect();
    rows.sort_by(|a, b| match (a.role_badge, b.role_badge) {
        (Some("Primary agent"), Some("Primary agent")) => a.model_id.cmp(&b.model_id),
        (Some("Primary agent"), _) => Ordering::Less,
        (_, Some("Primary agent")) => Ordering::Greater,
        _ => a.model_id.cmp(&b.model_id),
    });
    rows
}

/// Determines the role-pill text for a model based on which token-usage
/// category buckets it has non-zero tokens in. Mirrors the category
/// constants used by the existing credits-based breakdown
/// (`PRIMARY_AGENT_CATEGORY` / `FULL_TERMINAL_USE_CATEGORY`).
fn role_badge_for_model(model: &ModelTokenUsage) -> Option<&'static str> {
    let categories = [
        &model.warp_token_usage_by_category,
        &model.byok_token_usage_by_category,
        &model.custom_endpoint_token_usage_by_category,
    ];
    let has_category = |category: &str| {
        categories
            .iter()
            .any(|map| map.get(category).is_some_and(|&tokens| tokens > 0))
    };
    if has_category(PRIMARY_AGENT_CATEGORY) {
        Some("Primary agent")
    } else if has_category(FULL_TERMINAL_USE_CATEGORY) {
        Some("Full terminal use")
    } else {
        None
    }
}

/// Splits the per-agent rollup list into the rows to render now and the
/// count still hidden, honoring the truncation cap and "show all" state.
fn truncate_rollup_rows(
    entries: &[PerAgentCreditEntry],
    show_all: bool,
) -> (&[PerAgentCreditEntry], usize) {
    if show_all || entries.len() <= ROLLUP_TRUNCATION_CAP {
        (entries, 0)
    } else {
        (
            &entries[..ROLLUP_TRUNCATION_CAP],
            entries.len() - ROLLUP_TRUNCATION_CAP,
        )
    }
}

fn agent_row_color(entry: &PerAgentCreditEntry, theme: &WarpTheme) -> ColorU {
    match entry.avatar {
        AgentAvatar::Orchestrator => theme.ansi_fg_cyan(),
        AgentAvatar::Child => color_for_model(&entry.display_name),
    }
}

/// Computes the conversation- (or, when an orchestration rollup applies,
/// rollup-) wide total tokens and dollar cost, shared by the inference/agent
/// usage section's collapsed-summary text and its expanded "All models"/
/// "All agents" row so the two always agree.
fn total_usage_tokens_and_cost(
    conversation: &AIConversation,
    rollup: Option<&OrchestrationCreditRollup>,
) -> (Option<u64>, Option<f32>) {
    match rollup {
        Some(rollup) => (
            rollup.total_tokens.map(u64::from),
            rollup.total_cost_in_cents,
        ),
        None => {
            let total_tokens: u64 = conversation
                .token_usage()
                .iter()
                .map(|model| {
                    (model.warp_tokens + model.byok_tokens + model.custom_endpoint_tokens) as u64
                })
                .sum();
            (
                Some(total_tokens),
                conversation.usage_totals().cost_in_cents,
            )
        }
    }
}

/// Formats a raw token count using `k`/`M`-suffixed abbreviations above
/// 1,000 and 1,000,000 tokens respectively (e.g. `9.6k`, `1.6M`), matching
/// the Figma copy's token formatting.
fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.)
    } else if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.)
    } else {
        tokens.to_string()
    }
}

/// Returns the exact (unabbreviated, comma-separated) token count for a
/// tooltip, e.g. `"9,614 tokens"` -- `None` when `tokens` is small enough
/// that [`format_token_count`] wouldn't have abbreviated it in the first
/// place, since a tooltip repeating an already-exact "500 tokens" would be
/// redundant.
fn exact_token_count_tooltip(tokens: u64) -> Option<String> {
    (tokens >= 1000).then(|| format!("{} tokens", tokens.separate_with_commas()))
}

/// Wraps `content` in a hover tooltip showing `tooltip_text` below its
/// bottom-left corner, using the given (persistent, per-instance)
/// `hover_state` so the hover-in delay can actually fire -- see
/// `UsagePopoverView::hover_states`' docs for why persistence matters.
fn with_tooltip(
    hover_state: MouseStateHandle,
    content: Box<dyn Element>,
    tooltip_text: String,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Hoverable::new(hover_state, |state| {
        let mut stack = Stack::new().with_child(content);
        if state.is_hovered() {
            stack.add_positioned_overlay_child(
                render_tooltip_box(tooltip_text, appearance),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 4.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    })
    .finish()
}

/// Renders a small opaque tooltip box containing `text`. The background's
/// alpha channel is forced to fully opaque explicitly, rather than trusting
/// any theme color's own alpha: neither `Fill::into_solid()` nor
/// `coloru_with_opacity()` ever *force* full opacity -- they only preserve
/// or proportionally scale whatever alpha the color already has -- and
/// `theme.background()` (and, transitively through its blend chain,
/// `surface_1()`/`surface_2()`/`surface_3()`) can carry a reduced alpha
/// from the user's window-opacity/blur setting. Without forcing it here,
/// the box reads as translucent against whatever row happens to be behind
/// it.
fn render_tooltip_box(text: String, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    let surface = theme.surface_3().into_solid();
    let bg = ColorU::new(surface.r, surface.g, surface.b, 255);
    Container::new(
        Text::new(text, appearance.ui_font_family(), appearance.ui_font_size())
            .with_color(blended_colors::text_main(theme, bg))
            .with_selectable(false)
            .finish(),
    )
    .with_background_color(bg)
    .with_border(Border::all(1.).with_border_color(theme.outline().into_solid()))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
    .with_padding_left(8.)
    .with_padding_right(8.)
    .with_padding_top(4.)
    .with_padding_bottom(4.)
    .with_drop_shadow(
        DropShadow::new_with_standard_offset_and_spread(ColorU::new(0, 0, 0, 48))
            .with_offset(vec2f(0., 4.)),
    )
    .finish()
}

/// Formats a token count alongside its dollar cost, e.g. `"9.6k tokens /
/// $0.36"`. Per the pricing-transparency "do not show credits" decision,
/// this is the only value format used in the popover — credits are never
/// displayed. Either figure may be unknown (e.g. a rollup contributor
/// without a per-agent token/cost baseline): the two are joined with `/`
/// when both are known, and the popover falls back to whichever single
/// figure is available, or an em dash when neither is known. The dollar
/// figure is omitted entirely when `FeatureFlag::PricingTransparency` is
/// disabled.
pub(crate) fn format_tokens_and_cost(tokens: Option<u64>, cost_in_cents: Option<f32>) -> String {
    let token_text = tokens.map(|tokens| format!("{} tokens", format_token_count(tokens)));
    let cost_text = FeatureFlag::PricingTransparency
        .is_enabled()
        .then(|| cost_in_cents.map(|cost| format!("${:.2}", cost / 100.)))
        .flatten();
    match (token_text, cost_text) {
        (Some(tokens), Some(cost)) => format!("{tokens} / {cost}"),
        (Some(tokens), None) => tokens,
        (None, Some(cost)) => cost,
        (None, None) => "\u{2014}".to_string(),
    }
}

/// Formats a count alongside its dollar cost, e.g. `"3 searches / $0.02"`,
/// for breakdown rows whose unit isn't tokens (currently just web
/// searches). The dollar figure is omitted when `FeatureFlag::PricingTransparency`
/// is disabled, matching [`format_tokens_and_cost`].
fn format_count_and_cost(count: u32, unit: &str, cost_in_cents: f32) -> String {
    let count_text = format!("{count} {unit}");
    if !FeatureFlag::PricingTransparency.is_enabled() {
        return count_text;
    }
    format!("{count_text} / ${:.2}", cost_in_cents / 100.)
}

/// Formats a bare dollar cost, e.g. `"$0.36"`, for values with no
/// associated token/count figure (currently the platform fee and the
/// footer icon's hover tooltip). Shows an em dash when the cost is unknown
/// or `FeatureFlag::PricingTransparency` is disabled.
pub(crate) fn format_cost_only(cost_in_cents: Option<f32>) -> String {
    if !FeatureFlag::PricingTransparency.is_enabled() {
        return "\u{2014}".to_string();
    }
    match cost_in_cents {
        Some(cost) => format!("${:.2}", cost / 100.),
        None => "\u{2014}".to_string(),
    }
}

/// Renders a small rounded color swatch used to key a row to its bar
/// segment.
fn render_swatch(color: ColorU) -> Box<dyn Element> {
    Container::new(
        ConstrainedBox::new(Empty::new().finish())
            .with_width(SWATCH_SIZE)
            .with_height(SWATCH_SIZE)
            .finish(),
    )
    .with_background_color(color)
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(2.)))
    .finish()
}

/// Renders a full-width segmented bar. `segments` is a list of (color,
/// percentage) pairs; any remaining percentage up to 100 is filled with
/// `track_color`. The leading and trailing edges of the bar are rounded
/// (each visible segment's own edges stay square except at those two ends),
/// giving the overall bar a pill-like shape.
fn render_segmented_bar(segments: &[(ColorU, f32)], track_color: ColorU) -> Box<dyn Element> {
    let mut visible: Vec<(ColorU, f32)> = segments
        .iter()
        .copied()
        .filter(|(_, pct)| *pct > 0.)
        .collect();
    let used_pct: f32 = visible.iter().map(|(_, pct)| pct).sum();
    let remainder = (100. - used_pct).max(0.);
    if remainder > 0. {
        visible.push((track_color, remainder));
    }

    let end_radius = Radius::Pixels(BAR_HEIGHT / 2.);
    let last_index = visible.len().saturating_sub(1);
    let mut row = Flex::row();
    for (index, (color, pct)) in visible.iter().enumerate() {
        let mut corner_radius = CornerRadius::default();
        if index == 0 {
            corner_radius.merge(CornerRadius::with_left(end_radius));
        }
        if index == last_index {
            corner_radius.merge(CornerRadius::with_right(end_radius));
        }
        row.add_child(
            Expanded::new(
                *pct,
                Container::new(Empty::new().finish())
                    .with_background_color(*color)
                    .with_corner_radius(corner_radius)
                    .finish(),
            )
            .finish(),
        );
    }

    ConstrainedBox::new(row.finish())
        .with_height(BAR_HEIGHT)
        .finish()
}

fn render_label_value_row(label: &str, value: String, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    let background = theme.surface_2();
    let font_size = appearance.ui_font_size();
    space_between_row()
        .with_child(
            Text::new(label.to_string(), appearance.ui_font_family(), font_size)
                .with_color(blended_colors::text_sub(theme, background))
                .finish(),
        )
        .with_child(
            Text::new(value, appearance.ui_font_family(), font_size)
                .with_color(blended_colors::text_main(theme, background))
                .finish(),
        )
        .finish()
}

fn render_diffs_row(
    lines_added: i32,
    lines_removed: i32,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let background = theme.surface_2();
    let font_size = appearance.ui_font_size();
    space_between_row()
        .with_child(
            Text::new(
                "Diffs applied".to_string(),
                appearance.ui_font_family(),
                font_size,
            )
            .with_color(blended_colors::text_sub(theme, background))
            .finish(),
        )
        .with_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Text::new(
                        format!("+{lines_added}"),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(theme.ansi_fg_green())
                    .finish(),
                )
                .with_child(
                    Container::new(
                        Text::new(
                            format!("-{lines_removed}"),
                            appearance.ui_font_family(),
                            font_size,
                        )
                        .with_color(theme.ansi_fg_red())
                        .finish(),
                    )
                    .with_margin_left(6.)
                    .finish(),
                )
                .finish(),
        )
        .finish()
}

/// Renders a hyperlink-styled, non-chevron text link (used for "Show N
/// more" / "Show fewer").
fn render_text_link(
    label: String,
    mouse_state: MouseStateHandle,
    action: UsagePopoverAction,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let link_color = theme.ansi_fg_blue();
    let font_size = appearance.ui_font_size();
    let font_family = appearance.ui_font_family();
    Hoverable::new(mouse_state, move |_state| {
        Text::new(label.clone(), font_family, font_size)
            .with_color(link_color)
            .with_selectable(false)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(action.clone());
    })
    .finish()
}

#[cfg(test)]
#[path = "usage_popover_view_tests.rs"]
mod tests;
