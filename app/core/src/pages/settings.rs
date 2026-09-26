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
    /// Narrow windows show the menu and the page on separate screens.
    /// While set, the page covers the menu; the back row returns to it.
    pub(crate) narrow_page: bool,
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
        self.chat.drawer_open = false;
        self.chat.member_sheet_open = false;
        self.settings.plugin = None;
        self.settings.page = None;
        self.settings.narrow_page = false;
        self.refresh_settings_states();
        self.refresh_theme_list();
        if self.settings.open {
            return false;
        }
        // The screen holds no text fields; opening it over the keyboard
        // strands both.
        self.release_text_focus();
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
        self.settings.narrow_page = false;
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
    /// in tree order. A narrow page stands alone: the back row is 0 and
    /// the page rows follow from 1.
    pub(crate) fn settings_action(&mut self, index: usize) -> bool {
        use crate::menu::Action;
        if self.narrow_settings_page() {
            if index == 0 {
                let back = if self.settings.plugin.is_some() {
                    Action::SettingsPluginBack
                } else {
                    Action::SettingsNarrowBack
                };
                return self.perform(back);
            }
            let action = self
                .settings_page_items()
                .into_iter()
                .nth(index - 1)
                .map(|item| item.action);
            return match action {
                Some(action) => self.perform(action),
                // A stale index: the list changed under the press. Staying
                // put beats acting on the wrong row.
                None => true,
            };
        }
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

    /// Whether the settings page covers its menu: narrow windows only,
    /// once a category is chosen.
    pub(crate) fn narrow_settings_page(&self) -> bool {
        self.settings.open && self.panes() == crate::Panes::One && self.settings.narrow_page
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
                // Last, so the existing rows keep their indices.
                items.push(Item::new(
                    Action::InstallThemeFile,
                    "ファイルからインストール",
                ));
                items
            }
            SettingsCategory::Plugins => match &self.settings.plugin {
                None => {
                    let mut items: Vec<Item> = self
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
                        .collect();
                    // Last, so the existing rows keep their indices.
                    items.push(Item::new(
                        Action::InstallPluginFile,
                        "ファイルからインストール",
                    ));
                    items
                }
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
                vec![
                    Item::new(Action::ToggleFps, "FPSを表示").selected(self.show_fps),
                    Item::new(Action::ShareLog, "ログを共有"),
                ]
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
    /// Nav and page share one index space, in tree order. Narrow windows
    /// show the menu and the page on separate screens instead: side by side
    /// they fit neither.
    pub(crate) fn settings_screen(&self) -> UiNode {
        use crate::menu::Item;
        let nav = self.settings_nav_items();
        let page_items = self.settings_page_items();
        let hovered = self.hovered_item();

        // Narrow windows show the menu and the page on separate screens:
        // side by side they fit neither.
        if self.panes() == crate::Panes::One && !self.settings.narrow_page {
            return UiNode::new(NodeId::SettingsScreen).child(
                UiNode::new(NodeId::SettingsNav).child(crate::menu::rows(&nav, hovered, 0)),
            );
        }

        if self.narrow_settings_page() {
            let back = if self.settings.plugin.is_some() {
                // Up one level, like the plugin's own back row below.
                Item::new(crate::menu::Action::SettingsPluginBack, "← 設定")
            } else {
                Item::new(crate::menu::Action::SettingsNarrowBack, "← 設定")
            };
            let mut page = UiNode::new(NodeId::SettingsPage);
            for text in self.settings_page_texts() {
                page = page.child(UiNode::text(NodeId::PrimitiveText, text));
            }
            page = page.child(crate::menu::rows(std::slice::from_ref(&back), hovered, 0));
            if !page_items.is_empty() {
                page = page.child(crate::menu::rows(&page_items, hovered, 1));
            }
            if let Some(tree) = self.settings_page_embed() {
                page = page.child(tree);
            }
            return UiNode::new(NodeId::SettingsScreen).child(page);
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::chat::tests::{app, hit_of, press_menu};
    use crate::*;

    fn themes_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-theme-select-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn install_theme(root: &std::path::Path, dir_name: &str, id: &str, name: &str) {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("theme.json"),
            format!(
                r#"{{"manifest":{{"id":"{id}","name":"{name}","version":"1.0.0","abi":1}},"rules":[]}}"#
            ),
        )
        .unwrap();
    }

    /// Listing reads manifests, skips the broken, and sorts by folder.
    #[test]
    fn scan_lists_installs_and_skips_broken() {
        let root = themes_root("scan");
        install_theme(&root, "b-second", "dev.example.second", "Second");
        install_theme(&root, "a-first", "dev.example.first", "First");
        std::fs::create_dir_all(root.join("empty")).unwrap();
        let broken = root.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("theme.json"), "{broken").unwrap();

        let listed = scan_themes_in(&root);
        let ids: Vec<_> = listed.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["dev.example.first", "dev.example.second"]);
        assert_eq!(listed[0].name, "First");
    }

    /// Selecting applies and remembers; going standard forgets. The
    /// machine's own selection stays untouched.
    #[test]
    fn selecting_applies_and_standard_restores() {
        let root = themes_root("select");
        install_theme(&root, "wall", "dev.example.wall", "Wall");
        let mut a = Gumicord::demo_unthemed();
        a.settings.themes = scan_themes_in(&root);

        a.select_theme_in("dev.example.wall".to_owned(), Some(root.clone()));
        assert_eq!(
            a.theme_source,
            ThemeSource::Saved("dev.example.wall".to_owned())
        );
        assert_eq!(
            a.theme.as_ref().map(|t| t.manifest.id.as_str()),
            Some("dev.example.wall")
        );
        assert!(a.theme_path.is_some(), "hot reload must follow the switch");
        assert_eq!(
            load_active_id_in(&root).as_deref(),
            Some("dev.example.wall")
        );

        a.use_bundled_theme_in(Some(root.clone()));
        assert_eq!(a.theme_source, ThemeSource::Bundled);
        assert!(a.theme_path.is_none());
        assert_eq!(
            a.theme.as_ref().map(|t| t.manifest.id.as_str()),
            Some("dev.gumicord.midnight")
        );
        assert_eq!(load_active_id_in(&root), None);
    }

    /// A broken file keeps the current theme up, like a broken edit does.
    #[test]
    fn a_broken_theme_is_not_applied() {
        let root = themes_root("broken-select");
        install_theme(&root, "wall", "dev.example.wall", "Wall");
        std::fs::write(root.join("wall").join("theme.json"), "{broken").unwrap();
        let mut a = Gumicord::demo_unthemed();
        a.settings.themes = scan_themes_in(&root);
        assert!(a.settings.themes.is_empty(), "broken theme got listed");

        // Listed while good, broken before the press.
        install_theme(&root, "wall", "dev.example.wall", "Wall");
        a.settings.themes = scan_themes_in(&root);
        std::fs::write(root.join("wall").join("theme.json"), "{broken").unwrap();
        a.select_theme_in("dev.example.wall".to_owned(), Some(root.clone()));
        assert_eq!(a.theme_source, ThemeSource::Bundled, "broken theme applied");
        assert_eq!(load_active_id_in(&root), None, "broken theme remembered");
    }

    /// The theme page lists installs with the active one marked, and
    /// pressing a row switches.
    #[test]
    fn theme_rows_list_installs_with_the_active_marked() {
        use gumicord_uitree::State;
        let root = themes_root("rows");
        install_theme(&root, "wall", "dev.example.wall", "Wall");
        let mut a = Gumicord::demo_unthemed();
        a.match_ctx = MatchContext::new(1280.0);
        a.settings.themes = scan_themes_in(&root);
        a.settings.open = true;
        a.settings.category = crate::menu::SettingsCategory::Theme;

        // Nav takes 0-3; the standard row is 4, the install is 5.
        let press = |a: &mut Gumicord, index: u32| {
            a.pressed(&[Hit {
                id: NodeId::OverlayMenuItem,
                key: Some(Key::Index(index)),
                rect: gumicord_render::Rect::ZERO,
                clip: None,
            }])
        };
        let selected = |a: &Gumicord| {
            let mut out = Vec::new();
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                if n.id == NodeId::OverlayMenuItem
                    && n.states.contains(State::Selected)
                    && let Some(Key::Index(i)) = &n.key
                {
                    out.push(*i);
                }
            });
            out
        };
        // Bundled to start: the theme tab and the standard row show active.
        // Nav takes 0-3; the standard row is 4, the install is 5.
        assert_eq!(selected(&a), [2, 4]);
        assert!(press(&mut a, 5));
        assert_eq!(
            a.theme_source,
            ThemeSource::Saved("dev.example.wall".to_owned())
        );
        // Selecting rescans the folder, which is empty for this app;
        // put the test install back the way production's rescan keeps it.
        a.settings.themes = scan_themes_in(&root);
        assert_eq!(selected(&a), [2, 5], "active mark did not follow");
        assert!(press(&mut a, 4));
        assert_eq!(a.theme_source, ThemeSource::Bundled);
        assert_eq!(selected(&a), [2, 4]);
    }

    /// Startup falls back to bundled when the saved theme is gone.
    #[test]
    fn startup_falls_back_when_the_saved_theme_is_gone() {
        let root = themes_root("startup");
        // The environment override would win over everything below.
        let env_before = std::env::var(THEME_ENV).ok();
        unsafe { std::env::remove_var(THEME_ENV) };

        let (theme, path, source) = initial_theme_in(&root);
        assert_eq!(source, ThemeSource::Bundled);
        assert!(path.is_none());
        assert!(theme.is_some(), "bundled theme did not parse");

        install_theme(&root, "wall", "dev.example.wall", "Wall");
        save_active_id_in(&root, Some("dev.example.gone"));
        let (_, _, source) = initial_theme_in(&root);
        assert_eq!(source, ThemeSource::Bundled, "missing theme kept");

        save_active_id_in(&root, Some("dev.example.wall"));
        let (theme, path, source) = initial_theme_in(&root);
        assert_eq!(source, ThemeSource::Saved("dev.example.wall".to_owned()));
        assert!(path.is_some());
        assert_eq!(
            theme.as_ref().map(|t| t.manifest.id.as_str()),
            Some("dev.example.wall")
        );

        if let Some(v) = env_before {
            unsafe { std::env::set_var(THEME_ENV, v) }
        }
    }

    fn with_settings() -> Gumicord {
        let mut a = Gumicord::demo_unthemed();
        // Width-dependent branches read the last built frame; production
        // always builds before input, so tests say their width too.
        a.match_ctx = MatchContext::new(1280.0);
        assert!(a.open_settings(), "開かなかった");
        a
    }

    fn narrow_settings() -> Gumicord {
        let mut a = Gumicord::demo_unthemed();
        a.match_ctx = MatchContext::new(400.0);
        assert!(a.open_settings(), "開かなかった");
        a
    }

    fn settings_ids(a: &Gumicord) -> Vec<NodeId> {
        let mut out = Vec::new();
        a.build_tree(Panes::Four).walk(&mut |n, _| out.push(n.id));
        out
    }

    fn settings_texts(a: &Gumicord) -> Vec<String> {
        let mut out = Vec::new();
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveText {
                out.extend(n.content.as_text().map(str::to_owned));
            }
        });
        out
    }

    /// The screen carries the nav and the page, in that order, under the
    /// title bar that never leaves.
    #[test]
    fn opening_settings_shows_screen_nav_and_page() {
        let a = with_settings();
        let ids = settings_ids(&a);
        let screen = ids
            .iter()
            .position(|i| *i == NodeId::SettingsScreen)
            .expect("画面がない");
        let nav = ids
            .iter()
            .position(|i| *i == NodeId::SettingsNav)
            .expect("分類がない");
        let page = ids
            .iter()
            .position(|i| *i == NodeId::SettingsPage)
            .expect("中身がない");
        assert!(screen < nav && nav < page, "並びが逆");
        assert!(ids.contains(&NodeId::ChromeTitlebar), "題名欄が消えている");

        let closed = app();
        assert!(
            !settings_ids(&closed).contains(&NodeId::SettingsScreen),
            "閉じているのに出ている"
        );
    }

    /// Demo loads no plugins, and nothing failed to fetch.
    #[test]
    fn empty_lists_say_so() {
        let a = with_settings();
        let texts = settings_texts(&a).join("\n");
        assert!(texts.contains("プラグインはありません"), "{texts}");
        let mut theme = with_settings();
        press_menu(&mut theme, 2);
        let texts = settings_texts(&theme).join("\n");
        assert!(texts.contains("問題は起きていません"), "{texts}");
    }

    /// The rows share the menu's index space: 0 closes, 1 and 2 switch.
    #[test]
    fn settings_rows_route_by_index() {
        let mut a = with_settings();
        press_menu(&mut a, 2);
        assert_eq!(
            a.settings.category,
            crate::menu::SettingsCategory::Theme,
            "分類が変わらない"
        );
        press_menu(&mut a, 1);
        assert_eq!(
            a.settings.category,
            crate::menu::SettingsCategory::Plugins,
            "戻れない"
        );
        press_menu(&mut a, 0);
        assert!(!a.settings.open, "閉じない");
    }

    /// Support sits last: 3 opens it without moving 0, 1 and 2.
    #[test]
    fn support_category_opens_last_and_carries_share_log() {
        let mut a = with_settings();
        press_menu(&mut a, 3);
        assert_eq!(
            a.settings.category,
            crate::menu::SettingsCategory::Support,
            "増えた分類が開かない"
        );
        assert!(
            a.settings_page_items()
                .iter()
                .any(|i| i.action == crate::menu::Action::ShareLog),
            "共有の行がない"
        );
    }

    /// Support carries the FPS toggle first: pressing it flips the meter
    /// without leaving the screen, and the meter node rides the tree.
    #[test]
    fn support_carries_the_fps_toggle() {
        let mut a = with_settings();
        press_menu(&mut a, 3);
        assert!(
            a.settings_page_items()
                .iter()
                .any(|i| i.action == crate::menu::Action::ToggleFps),
            "FPS の行がない"
        );
        assert!(!a.show_fps);
        // Nav takes 0-3; the FPS row is 4.
        assert!(press_menu(&mut a, 4));
        assert!(a.show_fps, "押しても付かない");
        assert!(a.settings.open, "押したら閉じた");
        // A drawn frame's numbers reach the overlay on the next build.
        a.report_frame(gumicord_platform::FrameReport {
            fps: 59.0,
            frame_ms: 4.0,
            atlas_pages: 1,
            atlas_bytes: 16 * 1024 * 1024,
            nodes: 10,
            rects: 8,
            glyphs: 20,
            draw_calls: 3,
        });
        let mut found = 0;
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            if n.id == NodeId::OverlayFps {
                found += 1;
                assert!(n.anchor.is_some(), "右上に寄っていない");
                let text = n.content.as_text().unwrap_or_default();
                assert!(text.contains("fps"), "計測文でない: {text}");
                assert!(text.contains("アトラス"), "使用量が出ていない: {text}");
                assert!(text.contains("CPU"), "使用量が出ていない: {text}");
            }
        });
        assert_eq!(found, 1, "計測が出ていない");
        assert!(press_menu(&mut a, 4));
        assert!(!a.show_fps, "押しても消えない");
    }

    /// A stale index stays put instead of acting on the wrong row.
    #[test]
    fn a_stale_settings_index_keeps_the_screen() {
        let mut a = with_settings();
        press_menu(&mut a, 99);
        assert!(a.settings.open, "画面が消えた");
    }

    /// Nothing underneath is reachable while it is open. Presses that hit
    /// no row are swallowed instead of navigating behind it.
    #[test]
    fn nothing_underneath_is_reachable_while_settings_are_open() {
        let mut a = with_settings();
        let before = a.chat.selected_channel;
        assert!(!a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]));
        assert_eq!(a.chat.selected_channel, before, "下のチャンネルへ移動した");
        assert!(a.settings.open, "行外の押下で閉じた");
    }

    /// The gear and its press handler address the same slot, and the icon
    /// name exists in the registry: either link breaking leaves a dead,
    /// invisible button.
    #[test]
    fn the_gear_opens_settings() {
        assert!(
            gumicord_render::icon::lookup(SETTINGS_GEAR).is_some(),
            "歯車の絵がない"
        );
        let mut a = app();
        assert!(a.pressed(&[hit_of(
            NodeId::PrimitiveButton,
            Some(Key::Slot(SETTINGS_OPEN))
        )]));
        assert!(a.settings.open, "歯車で開かない");
    }

    /// The open tab's row carries Selected, so the theme can highlight it.
    #[test]
    fn the_open_tab_is_marked_selected() {
        use gumicord_uitree::State;
        let selected_index = |a: &Gumicord| {
            let mut out = Vec::new();
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                if n.id == NodeId::OverlayMenuItem
                    && n.states.contains(State::Selected)
                    && let Some(Key::Index(i)) = &n.key
                {
                    out.push(*i);
                }
            });
            out
        };
        let mut a = with_settings();
        assert_eq!(selected_index(&a), [1], "開いている分類行がない");
        press_menu(&mut a, 2);
        // 分類行に加え、標準のテーマ行も付く (demo は標準のはず)。
        assert_eq!(selected_index(&a), [2, 4], "移っていない");
    }

    /// Escape closes it; a press that hits no row does not, so a finger
    /// missing a row never loses where it was.
    #[test]
    fn escape_closes_settings_while_a_missed_press_keeps_them() {
        let mut a = with_settings();
        assert!(a.cancel_input());
        assert!(!a.settings.open, "Esc で閉じない");

        let mut b = with_settings();
        assert!(!b.pressed(&[]), "何も変わっていないはず");
        assert!(b.settings.open, "行外の押下で閉じた");
    }

    /// Narrow windows show the menu alone: nav and page side by side fit
    /// neither.
    #[test]
    fn narrow_settings_shows_the_menu_alone() {
        let a = narrow_settings();
        let mut ids = Vec::new();
        a.build_tree(Panes::One).walk(&mut |n, _| ids.push(n.id));
        assert!(ids.contains(&NodeId::SettingsNav), "分類がない");
        assert!(!ids.contains(&NodeId::SettingsPage), "中身まで出ている");
    }

    /// Choosing a category drills into its page with a back row.
    #[test]
    fn choosing_a_category_drills_into_its_page_when_narrow() {
        let mut a = narrow_settings();
        // Nav takes 0-3; 2 is the theme tab.
        assert!(a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(2)))]));
        assert!(a.settings.narrow_page, "掘り下げない");
        let mut ids = Vec::new();
        let mut labels = Vec::new();
        a.build_tree(Panes::One).walk(&mut |n, _| {
            ids.push(n.id);
            if n.id == NodeId::OverlayMenuItemLabel {
                labels.extend(n.content.as_text().map(str::to_owned));
            }
        });
        assert!(!ids.contains(&NodeId::SettingsNav), "分類が残っている");
        assert!(ids.contains(&NodeId::SettingsPage), "中身がない");
        assert!(labels.iter().any(|l| l == "← 設定"), "戻る行がない");
    }

    /// The narrow back row returns to the menu; the screen stays open.
    #[test]
    fn the_narrow_back_row_returns_to_the_menu() {
        let mut a = narrow_settings();
        assert!(a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(2)))]));
        assert!(a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(0)))]));
        assert!(a.settings.open, "閉じた");
        assert!(!a.settings.narrow_page, "戻らない");
        let mut ids = Vec::new();
        a.build_tree(Panes::One).walk(&mut |n, _| ids.push(n.id));
        assert!(ids.contains(&NodeId::SettingsNav), "分類に戻らない");
    }

    /// The close row still closes from the narrow menu.
    #[test]
    fn closing_from_the_narrow_menu() {
        let mut a = narrow_settings();
        assert!(a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(0)))]));
        assert!(!a.settings.open, "閉じない");
    }

    /// Both lists end with an install row; existing rows keep their indices.
    #[test]
    fn both_lists_end_with_an_install_row() {
        use crate::menu::Action;
        let mut a = with_settings();
        a.settings.category = crate::menu::SettingsCategory::Theme;
        assert!(
            matches!(
                a.settings_page_items().last().map(|r| &r.action),
                Some(Action::InstallThemeFile)
            ),
            "テーマ欄に導入行がない"
        );

        a.settings.category = crate::menu::SettingsCategory::Plugins;
        assert!(
            matches!(
                a.settings_page_items().last().map(|r| &r.action),
                Some(Action::InstallPluginFile)
            ),
            "プラグイン欄に導入行がない"
        );
    }

    /// A theme zip in memory, for installs that never touch a picker.
    fn theme_zip_bytes() -> Vec<u8> {
        use std::io::Write;
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        zip.start_file("theme.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"{"manifest": {"id": "dev.example.pack", "name": "Pack", "version": "1.0.0", "abi": 1}, "rules": []}"#,
        )
        .unwrap();
        zip.finish().unwrap();
        out.into_inner()
    }

    /// Installing theme bytes lands the theme, lists it, and says its name.
    #[test]
    fn installing_theme_bytes_lands_the_theme() {
        let dir = std::env::temp_dir().join("gumicord-install-ui-theme");
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = with_settings();
        a.themes_dir = Some(dir.clone());
        a.install_package_bytes_in(
            crate::install::InstallKind::Theme,
            "pack.zip",
            &theme_zip_bytes(),
            Some(dir.clone()),
            None,
        );
        assert!(dir.join("dev.example.pack").join("theme.json").is_file());
        assert!(
            a.toasts.back().is_some_and(|t| t.text.contains("Pack")),
            "入れたと言っていない"
        );
        assert!(
            a.settings.themes.iter().any(|t| t.id == "dev.example.pack"),
            "一覧に出ていない"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A broken package toasts instead of landing anything.
    #[test]
    fn a_broken_package_toasts_instead_of_landing() {
        let dir = std::env::temp_dir().join("gumicord-install-ui-broken");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = with_settings();
        a.install_package_bytes_in(
            crate::install::InstallKind::Theme,
            "wall.zip",
            b"not a zip",
            Some(dir.clone()),
            None,
        );
        assert!(
            a.toasts
                .back()
                .is_some_and(|t| t.text.contains("入れられなかった")),
            "失敗を言っていない"
        );
        assert!(dir.read_dir().unwrap().next().is_none(), "残骸がある");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plugin tarball in memory.
    fn plugin_tarball_bytes() -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut enc);
            for (name, body) in [
                (
                    "manifest.json",
                    &br#"{"id": "dev.example.side", "name": "Side", "version": "1.0.0"}"#[..],
                ),
                ("plugin.js", &b"globalThis.__gumicord_apply = (n) => n;"[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, body).unwrap();
            }
            tar.into_inner().unwrap();
        }
        enc.finish().unwrap()
    }

    /// Installing plugin bytes lands the directory; capabilities still ask
    /// through the usual dialog rather than granting silently.
    #[test]
    fn installing_plugin_bytes_lands_the_directory() {
        let root = std::env::temp_dir().join("gumicord-install-ui-plugins");
        let _ = std::fs::remove_dir_all(&root);
        let mut a = with_settings();
        a.install_package_bytes_in(
            crate::install::InstallKind::Plugin,
            "side.tar.gz",
            &plugin_tarball_bytes(),
            None,
            Some(root.clone()),
        );
        assert!(
            root.join("dev.example.side")
                .join("manifest.json")
                .is_file()
        );
        assert!(
            a.toasts.back().is_some_and(|t| t.text.contains("Side")),
            "入れたと言っていない"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The bundled theme always parses; a broken one starts up black.
    #[test]
    fn the_bundled_theme_parses() {
        let result = Theme::parse(DEFAULT_THEME);
        let errors: Vec<_> = result.errors().collect();
        assert!(errors.is_empty(), "同梱テーマに誤りがある: {errors:?}");
        assert!(result.is_applied());
    }

    /// Resolving the theme gives `app.window` a background.
    #[test]
    fn theme_reaches_the_tree() {
        let mut a = app();
        let cx = FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let window = &tree.children[0];
        assert_eq!(window.id, NodeId::AppWindow);
        assert!(
            window.style.background.is_some(),
            "app.window に背景が解決されていない"
        );
        // Inheritance reaches the leaves.
        let title = &window.children[0].children[0];
        assert_eq!(title.id, NodeId::ChromeTitlebarTitle);
        assert!(title.style.color.is_some(), "文字色が継承されていない");
    }
}
