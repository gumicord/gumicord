//! Transient layers over every screen: menus, dialogs, toasts, drawer and
//! sheets. Moved out of `super` in the pages split.

#[cfg(test)]
mod tests {
    use crate::*;
    use crate::pages::chat::tests::{app, hit_of, is_confirm, press_button, press_menu, swipe, with_menu};

    use gumicord_model::{Message, MessageId, User, UserId};

    fn live_app(timestamp: &str) -> Gumicord {
        let mut a = Gumicord::demo();
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 10;
        a.live
            .store_mut()
            .replace_guilds(vec![gumicord_model::Guild {
                id: 1u64.into(),
                name: "テスト".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: vec![gumicord_model::Channel {
                    id: 10u64.into(),
                    kind: gumicord_model::ChannelKind::GuildText,
                    name: Some("いっぱん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                }],
                roles: Vec::new(),
            }]);
        a.live.store_mut().set_backlog(
            ChannelId::from(10u64),
            vec![Message {
                id: MessageId::from(1u64),
                channel_id: ChannelId::from(10u64),
                guild_id: None,
                author: User {
                    id: UserId::from(7u64),
                    username: "nenneko".to_owned(),
                    global_name: None,
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                },
                content: "hi".to_owned(),
                timestamp: timestamp.to_owned(),
                edited_timestamp: None,
                pinned: false,
                attachments: Vec::new(),
                member: None,
                referenced_message: None,
                mentions: Vec::new(),
                mention_everyone: false,
            }],
        );
        a
    }

    fn tip_text(tip: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        tip.walk(&mut |n, _| {
            if n.id == NodeId::OverlayTooltip
                && let Some(s) = n.content.as_text()
            {
                out.push(s.to_owned());
            }
        });
        out
    }

    /// Hovering a timestamp shows the whole date, not the short hour.
    #[test]
    fn hovering_a_timestamp_shows_the_whole_date() {
        let mut a = live_app("2026-09-03T12:00:00+00:00");
        let rows = a.message_rows();
        assert!(!rows.is_empty(), "backlog did not take");
        a.hovered = Some((NodeId::ChatMessageHeaderTime, Some(Key::Id(1))));
        let tip = a.tooltip().expect("no tip");
        assert_eq!(
            tip_text(&tip),
            vec![format!("{} {}", rows[0].day, rows[0].time)]
        );
    }

    /// Anywhere else, and for dateless rows, there is nothing to add.
    #[test]
    fn hovering_anywhere_else_shows_nothing() {
        let mut a = live_app("2026-09-03T12:00:00+00:00");
        assert!(!a.message_rows().is_empty(), "backlog did not take");
        a.hovered = Some((NodeId::ChatMessageContent, Some(Key::Id(1))));
        assert!(a.tooltip().is_none());
        a.hovered = None;
        assert!(a.tooltip().is_none());

        let mut b = live_app("あとで");
        assert!(!b.message_rows().is_empty(), "backlog did not take");
        b.hovered = Some((NodeId::ChatMessageHeaderTime, Some(Key::Id(1))));
        assert!(b.tooltip().is_none(), "dateless rows add nothing");
    }

    /// Notices stack three deep, then expire.
    #[test]
    fn toasts_cap_and_expire() {
        let mut a = Gumicord::demo();
        for n in ["one", "two", "three", "four"] {
            a.notify_toast(n.to_owned());
        }
        assert_eq!(a.toasts.len(), 3);
        assert_eq!(a.toasts[0].text, "two");

        let now = gumicord_platform::now_unix();
        a.toasts.push_front(crate::menu::Toast {
            text: "old".to_owned(),
            until: now - 1,
        });
        assert!(a.prune_toasts(now));
        assert!(a.toasts.iter().all(|t| t.until > now));
        assert!(!a.prune_toasts(now), "nothing left to drop");
    }

    /// Toasts ride the tree only while shown.
    #[test]
    fn toasts_reach_the_tree_while_shown() {
        let mut a = Gumicord::demo();
        let shown = |a: &Gumicord| {
            let mut found = false;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                found = found || n.id == NodeId::OverlayToast;
            });
            found
        };
        assert!(!shown(&a));
        a.notify_toast("hi".to_owned());
        assert!(shown(&a));
    }

    /// The toast is a small centred box, not a fullscreen layer: fullscreen
    /// it paints its background over the chat it must not block.
    #[test]
    fn the_toast_is_a_small_centred_box() {
        let (w, h) = (1280.0, 800.0);
        let mut a = Gumicord::demo();
        a.notify_toast("テスト".to_owned());
        let viewport = gumicord_render::Size::new(w, h);
        let tree = a.build(&gumicord_platform::FrameCx {
            viewport,
            scale: 1.0,
        });
        let placed = gumicord_render::layout_for_test(&tree, viewport);
        let toast = placed
            .iter()
            .find(|(id, _)| *id == NodeId::OverlayToast)
            .map(|(_, r)| *r)
            .expect("トーストがない");
        assert!(toast.w < w && toast.h < h, "全画面を覆っている {toast:?}");
        assert!(
            (toast.x + toast.w / 2.0 - w / 2.0).abs() < 1.0,
            "横に寄っている {toast:?}"
        );
        assert!(
            (toast.y + toast.h / 2.0 - h / 2.0).abs() < 1.0,
            "縦に寄っている {toast:?}"
        );
    }

    /// Swipes do nothing while a dialog owns the input.
    #[test]
    fn swipes_are_ignored_under_overlays() {
        let mut a = with_menu();
        let hits = [hit_of(NodeId::ChatMessage, Some(Key::Id(7)))];
        assert!(!a.swiped(&hits, swipe(SwipeDir::Left, 300.0)));
        assert_eq!(a.chat.composing, Composing::New);
    }


    /// A press hits both layers, so without a rule it passes through and
    /// navigates to whatever the user meant to dismiss the menu over.
    #[test]
    fn nothing_underneath_is_reachable_while_the_menu_is_open() {
        let mut a = with_menu();
        let before = a.chat.selected_channel;
        // Include a hit on the channel underneath.
        let hits = [hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))];

        assert!(a.pressed(&hits), "閉じるという変化はある");
        assert!(a.floating.is_none(), "閉じていない");
        assert_eq!(a.chat.selected_channel, before, "下のチャンネルへ移動した");
    }


    /// A link under an open menu is dismissed with the menu, not opened:
    /// declining hands the press back to the dismissal path.
    #[test]
    fn a_link_press_declines_while_something_floats() {
        let mut a = with_menu();
        assert!(!a.link_pressed("https://example.com/"));

        let mut b = app();
        assert!(b.link_pressed("https://example.com/"));
    }


    /// Pressing an item runs it and closes the menu.
    ///
    /// Never writes to the clipboard: that would destroy whatever the person
    /// running the tests had copied.
    #[test]
    fn choosing_an_item_closes_the_menu() {
        let mut a = with_menu();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(
                crate::menu::Action::MarkRead(1),
                "既読にする",
            )],
        }));
        let hits = [hit_of(NodeId::OverlayMenuItem, Some(Key::Index(0)))];
        assert!(a.pressed(&hits));
        assert!(a.floating.is_none());
    }


    /// The menu floats above the composer, so escape stops there.
    #[test]
    fn esc_はメニューを先に閉じる() {
        let mut a = with_menu();
        a.chat.input_focused = true;

        assert!(a.cancel_input(), "何も起きなかった");
        assert!(a.floating.is_none(), "メニューが閉じていない");
        assert!(a.chat.input_focused, "入力欄のフォーカスまで外れた");

        assert!(a.cancel_input(), "2 回目でフォーカスが外れていない");
        assert!(!a.chat.input_focused);
    }

    // ═══════════════════════════════════════════════════════════════
    //  Signing out


    /// A menu holding only "delete".
    ///
    /// Built directly rather than through `message_menu`, which checks whether
    /// the message is ours and so offers nothing in demo mode. What matters
    /// here is what pressing it does.
    fn with_delete_menu() -> Gumicord {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(crate::menu::Action::Delete(1), "削除").danger()],
        }));
        // An observable marker that only clears once confirmed.
        a.chat.composing = Composing::Edit(1);
        a
    }


    /// One row among others in a menu, one line from its neighbours, and a
    /// deleted message cannot be recovered.
    #[test]
    fn one_press_of_delete_does_not_delete() {
        let mut a = with_delete_menu();
        assert!(press_menu(&mut a, 0));
        assert!(is_confirm(&a), "確認の窓が出ていない");
        assert_eq!(a.chat.composing, Composing::Edit(1), "確かめる前に消えている");
    }


    /// Cancelling does nothing and closes the dialog.
    #[test]
    fn cancelling_the_dialog_does_nothing() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CANCEL));
        assert!(a.floating.is_none(), "窓が閉じていない");
        assert_eq!(a.chat.composing, Composing::Edit(1), "やめたのに消えている");
    }


    /// Confirming is what actually deletes.
    #[test]
    fn confirming_the_dialog_deletes() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CONFIRM));
        assert!(a.floating.is_none(), "窓が閉じていない");
        // Deleting what is being edited also cancels the edit.
        assert_eq!(a.chat.composing, Composing::New, "消えていない");
    }


    /// A dialog represents an unmade decision; dismissing it on an outside
    /// press leaves the outcome ambiguous.
    #[test]
    fn clicking_outside_does_not_close_the_dialog() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(
            !a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]),
            "何かが変わってしまった"
        );
        assert!(is_confirm(&a), "外を押しただけで窓が消えた");
    }


    /// Escape closes it; no way out at all would be a dead end.
    #[test]
    fn escape_closes_the_dialog() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(a.cancel_input());
        assert!(a.floating.is_none(), "Esc で閉じない");
        assert_eq!(a.chat.composing, Composing::Edit(1), "Esc で消えている");
    }


    /// Confirming again would reopen the dialog forever.
    #[test]
    fn the_dialog_does_not_reappear() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        press_button(&mut a, crate::menu::button::CONFIRM);
        assert!(a.floating.is_none(), "窓がもう一度出ている");
    }


    /// Nothing underneath is reachable while it is open.
    #[test]
    fn nothing_underneath_is_reachable_while_the_dialog_is_open() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        let before = a.chat.selected_channel;

        a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]);
        assert_eq!(a.chat.selected_channel, before, "下のチャンネルへ移動した");
    }


    /// Confirming everything would stop the dialog being read at all.
    #[test]
    fn a_reversible_action_gets_no_dialog() {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(
                crate::menu::Action::MarkRead(1),
                "既読にする",
            )],
        }));
        press_menu(&mut a, 0);
        assert!(a.floating.is_none(), "既読にするだけで窓が出た");
    }

    // ═══════════════════════════════════════════════════════════════
    //  Settings screen


    /// The dialog's laid-out rectangles. Reading the theme's numbers does not
    /// show where things land.
    fn placed_confirm(w: f32, h: f32) -> Vec<(NodeId, gumicord_render::Rect)> {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        assert!(is_confirm(&a));

        // Demo mode has no body, and without the preview the widest row is
        // never measured.
        if let Some(crate::menu::Floating::Confirm(c)) = &mut a.floating {
            c.preview = crate::menu::preview_line(
                "おはようございます。今日はよろしくお願いします。長めの本文です",
            );
        }

        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(w, h),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        gumicord_render::layout_for_test(&tree, cx.viewport)
    }


    fn all_of(
        placed: &[(NodeId, gumicord_render::Rect)],
        id: NodeId,
    ) -> Vec<gumicord_render::Rect> {
        placed
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .collect()
    }


    fn one_of(placed: &[(NodeId, gumicord_render::Rect)], id: NodeId) -> gumicord_render::Rect {
        let all = all_of(placed, id);
        assert_eq!(all.len(), 1, "{id:?} が {} 個ある", all.len());
        all[0]
    }


    fn contains(outer: gumicord_render::Rect, inner: gumicord_render::Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.w <= outer.x + outer.w
            && inner.y + inner.h <= outer.y + outer.h
    }


    /// A button outside the dialog is visible but unpressable.
    #[test]
    fn the_dialog_contents_stay_inside_it() {
        let placed = placed_confirm(1280.0, 800.0);
        let modal = one_of(&placed, NodeId::OverlayModal);
        assert!(modal.w > 0.0 && modal.h > 0.0, "窓が潰れている {modal:?}");

        for id in [
            NodeId::OverlayModalTitle,
            NodeId::OverlayModalBody,
            NodeId::OverlayModalPreview,
            NodeId::OverlayModalActions,
        ] {
            let r = one_of(&placed, id);
            assert!(contains(modal, r), "{id:?} {r:?} が窓 {modal:?} から出た");
        }

        let buttons = all_of(&placed, NodeId::OverlayModalAction);
        assert_eq!(buttons.len(), 2, "ボタンが 2 つ無い");
        for b in &buttons {
            assert!(b.w > 0.0 && b.h > 0.0, "ボタンが潰れている {b:?}");
            assert!(contains(modal, *b), "ボタン {b:?} が窓 {modal:?} から出た");
        }

        // Otherwise what can be pressed and what can be read drift apart.
        for (label, button) in all_of(&placed, NodeId::OverlayModalActionLabel)
            .iter()
            .zip(&buttons)
        {
            assert!(
                contains(*button, *label),
                "文字 {label:?} がボタン {button:?} から出た"
            );
        }
    }


    /// Overlapping leaves one visible but unreachable.
    #[test]
    fn the_two_buttons_do_not_overlap() {
        let placed = placed_confirm(1280.0, 800.0);
        let b = all_of(&placed, NodeId::OverlayModalAction);
        assert_eq!(b.len(), 2);
        let (left, right) = (b[0], b[1]);
        assert!(
            left.x + left.w <= right.x + 0.01,
            "やめる {left:?} と 削除する {right:?} が重なっている"
        );
    }


    /// Centred, not placed at the press.
    #[test]
    fn the_dialog_is_centred_on_screen() {
        let (w, h) = (1280.0, 800.0);
        let modal = one_of(&placed_confirm(w, h), NodeId::OverlayModal);
        let cx = modal.x + modal.w / 2.0;
        let cy = modal.y + modal.h / 2.0;
        assert!((cx - w / 2.0).abs() < 1.0, "横にずれている {modal:?}");
        assert!((cy - h / 2.0).abs() < 1.0, "縦にずれている {modal:?}");
    }


    /// Overflowing at phone widths puts cancel out of reach.
    #[test]
    fn it_fits_in_a_narrow_window() {
        let (w, h) = (400.0, 700.0);
        let placed = placed_confirm(w, h);
        let modal = one_of(&placed, NodeId::OverlayModal);
        assert!(
            modal.x >= 0.0 && modal.x + modal.w <= w + 0.01,
            "画面から出た {modal:?} (幅 {w})"
        );
        assert!(
            modal.y >= 0.0 && modal.y + modal.h <= h + 0.01,
            "画面から出た {modal:?} (高さ {h})"
        );
        for b in all_of(&placed, NodeId::OverlayModalAction) {
            assert!(contains(modal, b), "ボタン {b:?} が窓 {modal:?} から出た");
        }
    }


    /// A permanent full-window layer would absorb every press.
    #[test]
    fn no_overlay_layer_is_built_while_nothing_is_open() {
        let has_layer = |a: &Gumicord| {
            let mut found = false;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                found |= n.id == NodeId::OverlayLayer;
            });
            found
        };
        assert!(!has_layer(&app()));
        assert!(has_layer(&with_menu()));
    }


    /// A press on nothing just closes what is open.
    #[test]
    fn right_clicking_empty_space_closes_the_menu() {
        let mut a = with_menu();
        assert!(a.context_menu(&[], (0.0, 0.0)));
        assert!(a.floating.is_none());
    }


    /// By width, not device: a narrowed desktop window reads better with a
    /// sheet.
    #[test]
    fn a_narrow_window_presents_the_menu_as_a_sheet() {
        use crate::menu::Present;
        assert_eq!(Panes::One.present(), Present::Sheet);
        assert_eq!(Panes::Two.present(), Present::Popover);
        assert_eq!(Panes::Four.present(), Present::Popover);
    }

}
