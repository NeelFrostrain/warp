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

use std::cmp::Ordering;
use std::collections::HashMap;

use pathfinder_color::ColorU;
use warp_core::ui::Icon;
use warp_core::ui::theme::WarpTheme;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Empty, Expanded, Flex,
    Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius, Text,
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
use crate::ai::blocklist::view_util::{format_credits, format_credits_with_cost};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::persistence::model::{
    FULL_TERMINAL_USE_CATEGORY, ModelTokenUsage, PRIMARY_AGENT_CATEGORY,
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
            appearance.ui_font_size() + 2.,
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

    /// "Total Usage" summary row shown above the model/agent usage
    /// section, using the orchestration rollup total when one applies.
    fn render_credits_summary_row(
        &self,
        conversation: &AIConversation,
        rollup: Option<&OrchestrationCreditRollup>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();

        let total_credits = rollup
            .map(|r| r.total_credits)
            .unwrap_or_else(|| conversation.credits_spent());

        // Total tokens across all models is only meaningful for the
        // single-conversation case; the orchestration rollup only tracks
        // credits per agent, so the parenthetical falls back to cost-only
        // when a rollup applies.
        let total_tokens: u32 = if rollup.is_some() {
            0
        } else {
            conversation
                .token_usage()
                .iter()
                .map(|model| model.warp_tokens + model.byok_tokens + model.custom_endpoint_tokens)
                .sum()
        };
        let cost_in_cents = conversation.usage_totals().cost_in_cents;
        let value_text = format_credits_with_cost(total_credits, Some(total_tokens), cost_in_cents);

        space_between_row()
            .with_child(
                Text::new(
                    "Total Usage".to_string(),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(blended_colors::text_sub(theme, background))
                .finish(),
            )
            .with_child(
                Text::new(value_text, appearance.ui_font_family(), font_size)
                    .with_color(blended_colors::text_main(theme, background))
                    .finish(),
            )
            .finish()
    }

    fn render_section_header(
        &self,
        label: &str,
        expanded: bool,
        mouse_state: MouseStateHandle,
        action: UsagePopoverAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let label_color = blended_colors::text_disabled(theme, background);
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let label = label.to_string();
        let overline_font_family = appearance.overline_font_family();
        let overline_font_size = appearance.overline_font_size();

        Hoverable::new(mouse_state, move |_state| {
            let label_element = Text::new(label.clone(), overline_font_family, overline_font_size)
                .with_color(label_color)
                .finish();
            let icon_element =
                ConstrainedBox::new(icon.to_warpui_icon(label_color.into()).finish())
                    .with_width(overline_font_size)
                    .with_height(overline_font_size)
                    .finish();
            space_between_row()
                .with_child(label_element)
                .with_child(icon_element)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
    }

    /// Renders either the per-model breakdown (default) or, when an
    /// orchestration rollup applies, the per-agent breakdown in its place
    /// (Surface 6 resolved decision 2).
    fn render_usage_breakdown_section(
        &self,
        conversation: &AIConversation,
        rollup: Option<&OrchestrationCreditRollup>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_spacing(8.);
        if let Some(rollup) = rollup {
            column.add_child(self.render_section_header(
                "AGENT USAGE",
                self.model_usage_section_expanded,
                self.model_usage_toggle_mouse_state.clone(),
                UsagePopoverAction::ToggleModelUsageSection,
                appearance,
            ));
            if self.model_usage_section_expanded {
                column.add_child(self.render_agent_rollup_rows(rollup, appearance));
            }
        } else {
            column.add_child(self.render_section_header(
                "MODEL USAGE",
                self.model_usage_section_expanded,
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

    fn render_model_usage_rows(
        &self,
        conversation: &AIConversation,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        // Token counts (with role-category info for the badges) and
        // per-model dollar cost come from two different underlying
        // structures, so join them here by model id.
        let costs_by_model: HashMap<String, f32> = conversation
            .total_token_usage()
            .into_iter()
            .map(|usage| (usage.model_id, usage.cost_in_cents))
            .collect();
        let rows = model_usage_rows(conversation.token_usage(), &costs_by_model);
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
                .with_child(
                    Text::new(
                        format_tokens_with_cost(
                            total_tokens,
                            conversation.usage_totals().cost_in_cents,
                        ),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_main(theme, background))
                    .finish(),
                )
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

    fn render_model_usage_row(
        &self,
        row: &ModelUsageRow,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.surface_2();
        let font_size = appearance.ui_font_size();
        let color = color_for_model(&row.model_id);

        let mut left = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(7.)
            .with_child(render_swatch(color))
            .with_child(
                Text::new(row.model_id.clone(), appearance.ui_font_family(), font_size)
                    .with_color(blended_colors::text_main(theme, background))
                    .soft_wrap(false)
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
            );
        if let Some(role) = row.role_badge {
            left.add_child(
                Container::new(render_role_pill(role, appearance))
                    .with_margin_left(6.)
                    .finish(),
            );
        }

        let value = Text::new(
            format_tokens_with_cost(row.tokens, row.cost_in_cents),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_sub(theme, background))
        .finish();

        space_between_row()
            .with_child(left.finish())
            .with_child(value)
            .finish()
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
                .with_child(
                    Text::new(
                        format_credits(rollup.total_credits),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_main(theme, background))
                    .finish(),
                )
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
        let value = Text::new(
            format_credits(entry.credits_spent),
            appearance.ui_font_family(),
            font_size,
        )
        .with_color(blended_colors::text_sub(theme, background))
        .finish();

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

        let mut column = Flex::column().with_spacing(8.);
        column.add_child(self.render_section_header(
            "RESPONSE TIME",
            self.response_time_section_expanded,
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
        column.add_child(self.render_credits_summary_row(
            conversation,
            rollup.as_ref(),
            appearance,
        ));
        column.add_child(self.render_usage_breakdown_section(
            conversation,
            rollup.as_ref(),
            appearance,
        ));
        column.add_child(self.render_tool_call_summary_section(conversation, appearance));
        column.add_child(self.render_response_time_section(conversation, appearance));

        let content = Container::new(column.finish())
            .with_background(theme.surface_2())
            .with_border(Border::all(1.).with_border_color(theme.outline().into_solid()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .with_uniform_padding(12.)
            .finish();

        ConstrainedBox::new(content)
            .with_width(POPOVER_WIDTH)
            .finish()
    }
}

impl Entity for UsagePopoverView {
    type Event = ();
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
        }
    }
}

/// One row of the per-model usage breakdown, aggregated across a model's
/// warp/byok/custom-endpoint token counts. Role badges mirror the existing
/// credits-based breakdown's category constants. `cost_in_cents` comes from
/// a separate per-model cost structure (`AIConversation::total_token_usage`)
/// joined in by model id, since the token/category source
/// (`AIConversation::token_usage`) doesn't carry cost.
struct ModelUsageRow {
    model_id: String,
    role_badge: Option<&'static str>,
    tokens: u64,
    cost_in_cents: Option<f32>,
}

/// Builds the sorted per-model row list from raw per-conversation token
/// usage. Rows are ordered primary-agent-first (matching the existing
/// credits-based breakdown's sort), then alphabetically by model id.
fn model_usage_rows(
    models: &[ModelTokenUsage],
    costs_by_model: &HashMap<String, f32>,
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
            Some(ModelUsageRow {
                model_id: model.model_id.clone(),
                role_badge,
                tokens,
                cost_in_cents: costs_by_model.get(&model.model_id).copied(),
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

/// Formats a raw token count using a `k`-suffixed abbreviation above 1000
/// tokens (e.g. `9.6k`), matching the Figma copy's token formatting.
fn format_token_count(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.)
    } else {
        tokens.to_string()
    }
}

/// Formats a token count alongside its dollar cost, e.g. `"9.6k tokens
/// ($0.36)"`. The cost suffix is omitted when `cost_in_cents` is `None`
/// (unknown, not zero) or when `FeatureFlag::PricingTransparency` is
/// disabled, matching the gating used by [`format_credits_with_cost`]
/// elsewhere in the usage surfaces.
fn format_tokens_with_cost(tokens: u64, cost_in_cents: Option<f32>) -> String {
    let token_text = format!("{} tokens", format_token_count(tokens));
    if !FeatureFlag::PricingTransparency.is_enabled() {
        return token_text;
    }
    match cost_in_cents {
        Some(cost) => format!("{token_text} (${:.2})", cost / 100.),
        None => token_text,
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

/// Renders the small pill/chip role badge (e.g. "Primary agent"), per
/// Surface 2 resolved decision 3 (pill component, not parenthetical text)
/// and resolved decision 4 (no casing normalization — pass through as-is).
fn render_role_pill(label: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Container::new(
        Text::new(
            label.to_string(),
            appearance.ui_font_family(),
            appearance.ui_font_size() - 2.,
        )
        .with_color(theme.background().into_solid())
        .finish(),
    )
    .with_background_color(blended_colors::neutral_6(theme))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(3.)))
    .with_horizontal_padding(4.)
    .with_vertical_padding(1.)
    .finish()
}

/// Renders a full-width segmented bar. `segments` is a list of (color,
/// percentage) pairs; any remaining percentage up to 100 is filled with
/// `track_color`.
fn render_segmented_bar(segments: &[(ColorU, f32)], track_color: ColorU) -> Box<dyn Element> {
    let mut row = Flex::row();
    let mut used_pct = 0.;
    for (color, pct) in segments {
        if *pct <= 0. {
            continue;
        }
        used_pct += pct;
        row.add_child(
            Expanded::new(
                *pct,
                Container::new(Empty::new().finish())
                    .with_background_color(*color)
                    .finish(),
            )
            .finish(),
        );
    }
    let remainder = (100. - used_pct).max(0.);
    if remainder > 0. {
        row.add_child(
            Expanded::new(
                remainder,
                Container::new(Empty::new().finish())
                    .with_background_color(track_color)
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
