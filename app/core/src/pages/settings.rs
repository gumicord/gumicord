//! The settings screen: categories left, page right.
//!
//! Owns its view state outright; rows are menu items so theme and press
//! routing already know them.

use gumicord_uitree::{NodeId, UiNode};

use super::super::{InstalledTheme, ThemeSource};

/// Where the settings screen stands. Closed most of the time; while open it
/// takes the screen's place under the title bar and owns every press below
/// it, like a menu.
#[derive(Debug, Clone, Default)]
pub(crate) struct SettingsView {
    pub(crate) open: bool,
    pub(crate) category: crate::menu::SettingsCategory,
    /// The plugin whose page is showing, if drilled in.
    pub(crate) plugin: Option<String>,
    /// That plugin's settings tree, read once per selection. The call
    /// blocks on the plugin worker, so frames never make it.
    pub(crate) page: Option<(String, UiNode)>,
    /// The plugin rows, refreshed on open, on actions, and on plugin
    /// events. Cached for the same reason as the page.
    pub(crate) states: Vec<gumicord_plugin::PluginState>,
    /// Installed themes, refreshed when the screen opens and after a
    /// switch. Scanning parses files, so frames never do it.
    pub(crate) themes: Vec<InstalledTheme>,
}

/// A plugin's state in the user's words. One place, so the list and the
/// page never disagree.
fn plugin_state_label(state: gumicord_plugin::PluginStateKind) -> &'static str {
    use gumicord_plugin::PluginStateKind;
    match state {
        PluginStateKind::Loaded => "有効",
        PluginStateKind::Disabled => "無効",
        PluginStateKind::Denied => "拒否",
        PluginStateKind::NeedsApproval => "未許可",
        PluginStateKind::LoadFailed => "読込失敗",
    }
}

impl crate::Gumicord {
    /// Opens the settings screen. The drill-in is reset; the category stays,
    /// like Discord remembering the section.
    pub(crate) fn open_settings(&mut self) -> bool {
        self.drawer_open = false;
        self.member_sheet_open = false;
        self.settings.plugin = None;
        self.settings.page = None;
        self.refresh_settings_states();
        self.refresh_theme_list();
        if self.settings.open {
            return false;
        }
        self.settings.open = true;
        true
    }

    /// Re-lists installed themes. Scanning parses files, so only on open
    /// and after a switch — never per frame.
    pub(crate) fn refresh_theme_list(&mut self) {
        self.settings.themes = self
            .themes_dir
            .as_deref()
            .map(crate::scan_themes_in)
            .unwrap_or_default();
    }

    pub(crate) fn close_settings(&mut self) -> bool {
        if !self.settings.open {
            return false;
        }
        self.settings.open = false;
        self.settings.plugin = None;
        self.settings.page = None;
        true
    }

    /// Re-reads the plugin rows. Blocking on the worker, so only on open,
    /// on actions, and on plugin events — never per frame.
    pub(crate) fn refresh_settings_states(&mut self) {
        self.settings.states = self.plugins.plugin_states();
    }

    pub(crate) fn select_settings_plugin(&mut self, id: String) {
        let page = self.plugins.settings_tree(&id);
        self.settings.plugin = Some(id.clone());
        self.settings.page = page.map(|tree| (id, tree));
    }

    /// A pressed settings row. The nav and the page share one index space,
    /// in tree order.
    pub(crate) fn settings_action(&mut self, index: usize) -> bool {
        let action = self
            .settings_nav_items()
            .into_iter()
            .chain(self.settings_page_items())
            .nth(index)
            .map(|item| item.action);
        match action {
            Some(action) => self.perform(action),
            // A stale index: the list changed under the press. Staying put
            // beats acting on the wrong row.
            None => true,
        }
    }

    pub(crate) fn settings_nav_items(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item, SettingsCategory};
        let mut items = vec![Item::new(Action::CloseSettings, "閉じる").icon("close")];
        for category in [
            SettingsCategory::Plugins,
            SettingsCategory::Theme,
            SettingsCategory::Support,
        ] {
            items.push(
                Item::new(Action::SettingsCategory(category), category.label())
                    .selected(self.settings.category == category),
            );
        }
        items
    }

    pub(crate) fn settings_page_items(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item, SettingsCategory};
        use gumicord_plugin::PluginStateKind;
        match self.settings.category {
            SettingsCategory::Theme => {
                let mut items = vec![
                    Item::new(Action::UseBundledTheme, "標準のテーマ")
                        .selected(self.theme_source == ThemeSource::Bundled),
                ];
                items.extend(self.settings.themes.iter().map(|t| {
                    // Names and versions are data, not words, so one line
                    // stays splittable later (ADR-0010).
                    Item::new(
                        Action::SelectTheme(t.id.clone()),
                        format!("{} {}", t.name, t.version),
                    )
                    .selected(self.theme_source == ThemeSource::Saved(t.id.clone()))
                }));
                items
            }
            SettingsCategory::Plugins => match &self.settings.plugin {
                None => self
                    .settings
                    .states
                    .iter()
                    .map(|p| {
                        // One-line rows assemble name, version and state;
                        // split them when the message table arrives (ADR-0010).
                        Item::new(
                            Action::SelectSettingsPlugin(p.id.clone()),
                            format!(
                                "{} {}（{}）",
                                p.name,
                                p.version,
                                plugin_state_label(p.state)
                            ),
                        )
                    })
                    .collect(),
                Some(id) => {
                    let mut items = vec![Item::new(Action::SettingsPluginBack, "← プラグイン")];
                    if let Some(p) = self.settings.states.iter().find(|p| &p.id == id) {
                        // The approval dialog owns unapproved plugins; the
                        // re-approve row below is their way back to it.
                        match p.state {
                            PluginStateKind::Loaded => {
                                items.push(Item::new(
                                    Action::DisablePlugin(id.clone()),
                                    "無効にする",
                                ));
                                items.push(Item::new(
                                    Action::ReloadPlugin(id.clone()),
                                    "再読み込みする",
                                ));
                            }
                            PluginStateKind::Disabled | PluginStateKind::LoadFailed => {
                                items.push(Item::new(
                                    Action::EnablePlugin(id.clone()),
                                    "有効にする",
                                ));
                            }
                            PluginStateKind::Denied | PluginStateKind::NeedsApproval => {}
                        }
                        items.push(Item::new(
                            Action::ReapprovePlugin(id.clone()),
                            "許可をやり直す",
                        ));
                    }
                    items
                }
            },
            SettingsCategory::Support => {
                vec![Item::new(Action::ShareLog, "ログを共有")]
            }
        }
    }

    /// The page's display-only lines: status, capabilities, warnings. Rows
    /// stay rows; these are what rows cannot say.
    pub(crate) fn settings_page_texts(&self) -> Vec<String> {
        use crate::menu::SettingsCategory;
        match self.settings.category {
            SettingsCategory::Theme => {
                let warnings = self.assets.warnings();
                if warnings.is_empty() {
                    vec!["画像の取得で問題は起きていません。".to_owned()]
                } else {
                    warnings
                }
            }
            SettingsCategory::Plugins => match &self.settings.plugin {
                None if self.settings.states.is_empty() => {
                    vec!["プラグインはありません。".to_owned()]
                }
                None => Vec::new(),
                Some(id) => match self.settings.states.iter().find(|p| &p.id == id) {
                    None => vec!["このプラグインはもうありません。".to_owned()],
                    Some(p) => {
                        // Name and state stay on separate lines: word order
                        // moves by language, so they must not share one
                        // sentence (ADR-0010).
                        let mut out = vec![
                            format!("{} {}", p.name, p.version),
                            plugin_state_label(p.state).to_owned(),
                        ];
                        if p.capabilities.is_empty() {
                            out.push("権限を求めません。".to_owned());
                        } else {
                            out.push(format!(
                                "求める権限:\n{}",
                                crate::capability_bullets(&p.capabilities)
                            ));
                        }
                        let page_missing =
                            self.settings.page.as_ref().is_none_or(|(pid, _)| pid != id);
                        if p.has_settings && page_missing {
                            out.push("設定ページを読み込めませんでした。".to_owned());
                        }
                        out
                    }
                },
            },
            SettingsCategory::Support => {
                vec!["不具合の報告に使う。記録にトークンは含まれない。".to_owned()]
            }
        }
    }

    /// The selected plugin's own page, if it declared and delivered one.
    pub(crate) fn settings_page_embed(&self) -> Option<UiNode> {
        let id = self.settings.plugin.as_ref()?;
        let (pid, tree) = self.settings.page.as_ref()?;
        (pid == id).then(|| tree.clone())
    }

    /// The settings screen, Discord-style: categories left, page right. Rows
    /// are menu items, so the theme and the press routing already know them.
    /// Nav and page share one index space, in tree order.
    pub(crate) fn settings_screen(&self) -> UiNode {
        let nav = self.settings_nav_items();
        let page_items = self.settings_page_items();
        let hovered = self.hovered_item();

        let mut page = UiNode::new(NodeId::SettingsPage);
        for text in self.settings_page_texts() {
            page = page.child(UiNode::text(NodeId::PrimitiveText, text));
        }
        if !page_items.is_empty() {
            page = page.child(crate::menu::rows(&page_items, hovered, nav.len()));
        }
        if let Some(tree) = self.settings_page_embed() {
            page = page.child(tree);
        }

        UiNode::new(NodeId::SettingsScreen)
            .child(UiNode::new(NodeId::SettingsNav).child(crate::menu::rows(&nav, hovered, 0)))
            .child(page)
    }
}
