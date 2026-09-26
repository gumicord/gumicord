//! Chat page tests, plus the small builders other pages' tests share.
use super::*;
use crate::*;

pub(crate) fn app() -> Gumicord {
    Gumicord::demo()
}

// ═══════════════════════════════════════════════════════════════
//  Replying and editing

pub(crate) fn swipe(dir: SwipeDir, x: f32) -> Swipe {
    Swipe::Point { dir, x, y: 400.0 }
}

pub(crate) fn narrow() -> Gumicord {
    let mut a = app();
    a.match_ctx = MatchContext::new(400.0);
    a
}

pub(crate) fn hit_of(id: NodeId, key: Option<Key>) -> Hit {
    Hit {
        id,
        key,
        rect: gumicord_render::Rect::ZERO,
        clip: None,
    }
}

/// A press at a rectangle, for surface containment: the drawer and the
/// sheet only act on hits inside their own rectangle.
pub(crate) fn hit_at(id: NodeId, key: Option<Key>, x: f32, y: f32, w: f32, h: f32) -> Hit {
    Hit {
        id,
        key,
        rect: gumicord_render::Rect::new(x, y, w, h),
        clip: None,
    }
}

/// Runs the drawer slide to its landing: closing flips the flag only
/// when the slide arrives, like production frames do.
pub(crate) fn settle_drawer(a: &mut Gumicord) {
    a.advance_drawer(std::time::Instant::now() + std::time::Duration::from_secs(1));
}

/// Runs the sheet slide to its landing the same way.
pub(crate) fn settle_sheet(a: &mut Gumicord) {
    a.advance_sheet(std::time::Instant::now() + std::time::Duration::from_secs(1));
}

/// A menu on a narrow window, presented as a sheet.
pub(crate) fn narrow_menu() -> Gumicord {
    let mut a = narrow();
    let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
    assert!(
        a.context_menu(std::slice::from_ref(&msg), (10.0, 20.0)),
        "メニューが開かなかった"
    );
    assert!(
        matches!(a.floating, Some(crate::menu::Floating::Menu(_))),
        "品物がない"
    );
    a
}

pub(crate) fn with_menu() -> Gumicord {
    let mut a = app();
    let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
    assert!(
        a.context_menu(std::slice::from_ref(&msg), (10.0, 20.0)),
        "メニューが開かなかった"
    );
    a
}

pub(crate) fn press_menu(a: &mut Gumicord, index: u32) -> bool {
    a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(index)))])
}

pub(crate) fn press_button(a: &mut Gumicord, index: usize) -> bool {
    a.pressed(&[hit_of(
        NodeId::OverlayModalAction,
        Some(Key::Index(index as u32)),
    )])
}

pub(crate) fn is_confirm(a: &Gumicord) -> bool {
    matches!(a.floating, Some(crate::menu::Floating::Confirm(_)))
}

fn built(a: &mut Gumicord) {
    let cx = gumicord_platform::FrameCx {
        viewport: gumicord_render::Size::new(1280.0, 800.0),
        scale: 1.0,
    };
    a.build(&cx);
}

/// Sending a new message while meaning to edit cannot be undone, so the
/// mode has to be visible.
#[test]
fn replying_and_editing_are_visible_on_screen() {
    let bar = |c: Composing| {
        let mut a = app();
        a.chat.composing = c;
        let mut out = None;
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            if n.id == NodeId::ChatInputToolbar {
                out = n.key.clone();
            }
        });
        out
    };
    assert_eq!(bar(Composing::New), None, "何もしていないのに出ている");
    assert_eq!(bar(Composing::Reply(1)), Some(Key::Slot("reply")));
    assert_eq!(bar(Composing::Edit(1)), Some(Key::Slot("edit")));
}

/// This happened: a later `primitive.button` rule added horizontal
/// padding, which pushed the icon outside its 20-square box and left an
/// empty dark box beside it. Reading the theme's numbers does not catch
/// it; the laid-out rectangles do.
#[test]
fn the_cancel_icon_stays_inside_its_box() {
    let mut a = app();
    a.chat.composing = Composing::Reply(1);
    let cx = gumicord_platform::FrameCx {
        viewport: gumicord_render::Size::new(1280.0, 800.0),
        scale: 1.0,
    };
    let tree = a.build(&cx);
    let placed = gumicord_render::layout_for_test(&tree, cx.viewport);

    let find = |id| {
        placed
            .iter()
            .rev()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{id:?} が置かれていない"))
    };
    let button = find(NodeId::PrimitiveButton);
    let icon = find(NodeId::PrimitiveIcon);

    assert!(
        button.w > 0.0 && button.h > 0.0,
        "箱が潰れている {button:?}"
    );
    assert!(
        icon.x >= button.x
            && icon.y >= button.y
            && icon.x + icon.w <= button.x + button.w
            && icon.y + icon.h <= button.y + button.h,
        "絵 {icon:?} が箱 {button:?} からはみ出している"
    );
}

/// Escape works too, but without a visible way out this looks like a
/// state with no exit.
#[test]
fn a_cancel_button_appears_while_replying() {
    let cancel = |c: Composing| {
        let mut a = app();
        a.chat.composing = c;
        let mut found = false;
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            found |= n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot(CANCEL_COMPOSING));
        });
        found
    };
    assert!(!cancel(Composing::New), "何もしていないのに出ている");
    assert!(cancel(Composing::Reply(1)));
    assert!(cancel(Composing::Edit(1)));
}

/// The reply-mention switch shows only while replying, naming its state.
#[test]
fn the_mention_switch_appears_only_while_replying() {
    fn label(a: &mut Gumicord) -> Option<String> {
        let mut out = None;
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot(REPLY_MENTION)) {
                for c in &n.children {
                    if let gumicord_uitree::Content::Text(t) = &c.content {
                        out = Some(t.clone());
                    }
                }
            }
        });
        out
    }
    let mut replying = app();
    replying.chat.composing = Composing::Reply(1);
    replying.chat.reply_mention = true;
    assert_eq!(label(&mut replying).as_deref(), Some("@ON"));
    replying.chat.reply_mention = false;
    assert_eq!(label(&mut replying).as_deref(), Some("@OFF"));

    let mut editing = app();
    editing.chat.composing = Composing::Edit(1);
    assert_eq!(label(&mut editing), None);

    let mut fresh = app();
    fresh.chat.composing = Composing::New;
    assert_eq!(label(&mut fresh), None);
}

/// Pressing the switch flips whether the reply notifies its target.
#[test]
fn pressing_the_mention_switch_flips_the_flag() {
    let mut a = app();
    a.chat.composing = Composing::Reply(1);
    assert!(a.chat.reply_mention);
    let hits = [hit_of(
        NodeId::PrimitiveButton,
        Some(Key::Slot(REPLY_MENTION)),
    )];
    assert!(a.pressed(&hits));
    assert!(!a.chat.reply_mention);
    assert!(a.pressed(&hits));
    assert!(a.chat.reply_mention);
}

/// The switch does nothing outside a reply; the button is not even built.
#[test]
fn pressing_the_mention_switch_outside_a_reply_changes_nothing() {
    let mut a = app();
    a.chat.composing = Composing::Edit(1);
    let before = a.chat.reply_mention;
    let hits = [hit_of(
        NodeId::PrimitiveButton,
        Some(Key::Slot(REPLY_MENTION)),
    )];
    a.pressed(&hits);
    assert_eq!(a.chat.reply_mention, before);
    assert_eq!(a.chat.composing, Composing::Edit(1));
}

/// Cancelling a reply keeps the draft; cancelling an edit does not, since
/// the field held the original message rather than anything typed.
#[test]
fn cancelling_a_reply_keeps_the_draft_but_cancelling_an_edit_clears_it() {
    let press = |c: Composing| {
        let mut a = app();
        a.chat.composing = c;
        a.chat.input.insert("書いた文");
        let hits = [hit_of(
            NodeId::PrimitiveButton,
            Some(Key::Slot(CANCEL_COMPOSING)),
        )];
        assert!(a.pressed(&hits), "何も起きなかった");
        assert_eq!(a.chat.composing, Composing::New, "やめていない");
        a.chat.input.text().to_owned()
    };
    assert_eq!(press(Composing::Reply(1)), "書いた文", "返信で消えた");
    assert_eq!(press(Composing::Edit(1)), "", "編集で残った");
}

/// If the slot constant is read as a binding rather than a pattern, every
/// button falls through here. It looks identical until one is pressed.
#[test]
fn another_button_does_not_cancel() {
    let mut a = app();
    a.chat.composing = Composing::Reply(1);
    let hits = [hit_of(NodeId::PrimitiveButton, Some(Key::Slot("その他")))];

    a.pressed(&hits);
    assert_eq!(
        a.chat.composing,
        Composing::Reply(1),
        "別のボタンで取り消された"
    );
}

/// Escape cancels the reply or edit before discarding the draft; both at
/// once leaves it unclear which was lost.
#[test]
fn esc_は返信をやめてから閉じる() {
    let mut a = app();
    a.chat.input_focused = true;
    a.chat.composing = Composing::Reply(1);
    a.chat.input.insert("書きかけ");

    assert!(a.cancel_input());
    assert_eq!(a.chat.composing, Composing::New, "返信のままである");
    assert!(a.chat.input_focused, "フォーカスまで外れた");
}

/// Clearing the field and pressing enter must not destroy the message.
#[test]
fn submitting_an_empty_field_does_nothing() {
    let mut a = app();
    a.chat.composing = Composing::Edit(1);
    assert!(!a.submit());
    assert_eq!(a.chat.composing, Composing::Edit(1), "編集をやめてしまった");
}

/// Sending returns to composing a new message, or the next one is a reply
/// too.
#[test]
fn submitting_returns_to_composing_a_new_message() {
    let mut a = app();
    a.chat.composing = Composing::Reply(1);
    a.chat.input.insert("やあ");
    assert!(a.submit());
    assert_eq!(a.chat.composing, Composing::New);
}

/// The server would return 403 anyway, but not offering it comes first.
#[test]
fn someone_elses_message_offers_neither_edit_nor_delete() {
    use crate::menu::Action;
    // Demo mode is signed out, so nothing is ours.
    let a = app();
    let items = a.message_menu(1);
    assert!(
        !items
            .iter()
            .any(|i| matches!(i.action, Action::Edit(_) | Action::Delete(_))),
        "他人の発言に編集か削除が出ている"
    );
    // Reply is offered on anyone's message.
    assert!(items.iter().any(|i| matches!(i.action, Action::Reply(_))));
}

/// The composer overlaps the message list, so it is checked first.
#[test]
fn the_input_field_gets_the_input_menu() {
    use crate::menu::Action;
    let mut a = app();
    let hits = [
        hit_of(NodeId::ChatInputField, None),
        hit_of(NodeId::ChatMessage, Some(Key::Id(1))),
    ];
    assert!(a.context_menu(&hits, (0.0, 0.0)));

    let items = a.floating.as_ref().expect("開いていない").items();
    assert!(
        items.iter().any(|i| i.action == Action::Paste),
        "発言のメニューが出ている"
    );
}

/// Only what would do something.
#[test]
fn cut_and_copy_are_absent_without_a_selection() {
    use crate::menu::Action;
    let mut a = app();
    let has = |a: &Gumicord, want: Action| a.field_menu().iter().any(|i| i.action == want);

    assert!(!has(&a, Action::CopySelection));
    assert!(!has(&a, Action::SelectAll), "空なのに全選択が出ている");
    assert!(has(&a, Action::Paste), "貼り付けはいつでも出る");

    a.chat.input.insert("あいう");
    assert!(has(&a, Action::SelectAll));
    assert!(!has(&a, Action::CopySelection), "まだ選んでいない");

    a.chat.input.select_all();
    assert!(has(&a, Action::CopySelection));
    assert!(has(&a, Action::Cut));
}

// ═══════════════════════════════════════════════════════════════
//  Touch: swipes, drawer, member sheet

/// A message swiped left starts a reply, like the menu does.
#[test]
fn swipe_left_on_a_message_starts_a_reply() {
    let mut a = app();
    let hits = [hit_of(NodeId::ChatMessage, Some(Key::Id(7)))];
    assert!(a.swiped(&hits, swipe(SwipeDir::Left, 300.0)));
    assert_eq!(a.chat.composing, Composing::Reply(7));
    assert!(a.chat.input_focused, "入力欄に焦点がない");
    assert_eq!(a.chat.a11y_message, Some(7));
}

/// Flicking the drawer away closes it; flicking up scrolls instead.
#[test]
fn flicking_the_drawer_away_closes_it() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let drawer = hit_at(NodeId::OverlayDrawer, None, 0.0, 0.0, 280.0, 800.0);
    assert!(a.swiped(std::slice::from_ref(&drawer), swipe(SwipeDir::Left, 100.0)));
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "閉じない");

    assert!(a.open_drawer());
    assert!(!a.swiped(std::slice::from_ref(&drawer), swipe(SwipeDir::Up, 100.0)));
    assert!(a.chat.drawer_open, "上の払いで閉じた");
}

/// A swipe starting behind the drawer dismisses it without replying
/// through it.
#[test]
fn a_swipe_behind_the_drawer_dismisses_without_replying() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let behind = hit_at(
        NodeId::ChatMessage,
        Some(Key::Id(7)),
        300.0,
        400.0,
        80.0,
        50.0,
    );
    assert!(a.swiped(&[behind], swipe(SwipeDir::Left, 320.0)));
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "閉じていない");
    assert_eq!(a.chat.composing, Composing::New, "裏へ返信した");
}

/// Flicking the member sheet away closes it; flicking up does not.
#[test]
fn flicking_the_member_sheet_away_closes_it() {
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let sheet = hit_at(NodeId::OverlaySheet, None, 0.0, 400.0, 400.0, 400.0);
    assert!(a.swiped(std::slice::from_ref(&sheet), swipe(SwipeDir::Down, 200.0)));
    assert!(a.chat.member_sheet_open, "払った瞬間に消えた");
    settle_sheet(&mut a);
    assert!(!a.chat.member_sheet_open, "閉じない");

    assert!(a.open_member_sheet());
    assert!(!a.swiped(std::slice::from_ref(&sheet), swipe(SwipeDir::Up, 200.0)));
    assert!(a.chat.member_sheet_open, "上の払いで閉じた");
}

/// Opening starts the drawer shut and coasts it in over time.
#[test]
fn opening_coasts_the_drawer_in_over_time() {
    use std::time::{Duration, Instant};
    let mut a = narrow();
    let start = Instant::now();
    assert!(a.open_drawer());
    assert_eq!(a.chat.drawer_slide, 0.0, "開いた瞬間にいる");
    assert!(a.advance_drawer(start + Duration::from_millis(110)));
    let mid = a.chat.drawer_slide;
    assert!(mid > 0.0 && mid < 1.0, "途中にいない: {mid}");
    assert!(!a.advance_drawer(start + Duration::from_secs(1)));
    assert_eq!(a.chat.drawer_slide, 1.0);
    assert!(a.chat.drawer_open, "着いたのに閉じた");
}

/// Closing coasts out and flips the flag only on landing.
#[test]
fn closing_coasts_out_and_lands_shut() {
    use std::time::{Duration, Instant};
    let mut a = narrow();
    assert!(a.open_drawer());
    settle_drawer(&mut a);
    assert!(a.close_drawer());
    assert!(a.chat.drawer_open, "閉じ始めに消えた");
    assert!(!a.advance_drawer(Instant::now() + Duration::from_secs(1)));
    assert!(!a.chat.drawer_open, "着いたのに残っている");
    assert_eq!(a.chat.drawer_slide, 0.0);
}

/// Reopening mid-close swings back instead of refusing.
#[test]
fn reopening_mid_close_swings_back() {
    let mut a = narrow();
    assert!(a.open_drawer());
    settle_drawer(&mut a);
    assert!(a.close_drawer());
    assert!(a.open_drawer(), "閉じ途中に開けない");
    assert!(a.chat.drawer_open, "開き直していない");
}

/// The finger drives the slide; letting go coasts to a side.
#[test]
fn the_finger_drives_the_slide_and_letting_go_coasts() {
    // Slow release past halfway finishes opening (absolute from the start).
    let mut a = narrow();
    assert!(a.drawer_drag_start());
    assert!(a.chat.drawer_drag, "掴んでいない");
    assert!(a.drawer_drag_move(140.0, 280.0));
    assert_eq!(a.chat.drawer_slide, 0.5);
    assert!(a.drawer_drag_move(168.0, 280.0));
    assert!(a.drawer_drag_end(0.0));
    assert!(!a.chat.drawer_drag, "離したのに掴んだまま");
    assert_eq!(a.chat.drawer_target, 1.0, "半分過ぎたのに戻る");

    // A fast flick left falls back whatever the progress.
    let mut b = narrow();
    assert!(b.drawer_drag_start());
    assert!(b.drawer_drag_move(200.0, 280.0));
    assert!(b.drawer_drag_end(-1000.0));
    assert_eq!(b.chat.drawer_target, 0.0, "払ったのに開く");

    // A fast flick right opens from anywhere.
    let mut c = narrow();
    assert!(c.drawer_drag_start());
    assert!(c.drawer_drag_move(10.0, 280.0));
    assert!(c.drawer_drag_end(1000.0));
    assert_eq!(c.chat.drawer_target, 1.0, "払ったのに戻る");
}

/// A coasting drawer wakes the loop until it lands.
#[test]
fn a_coasting_drawer_wakes_the_loop_until_it_lands() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let wake = a.next_frame_in().expect("寝てしまう");
    assert!(wake <= std::time::Duration::from_millis(16), "{wake:?}");
    settle_drawer(&mut a);
    // Landed and idle: nothing to wake for without the overlay tick.
    assert_eq!(a.next_frame_in(), None);
}

/// The platform-facing trait path drives the same state machine the
/// gestures use; it must not recurse into itself.
#[test]
fn the_trait_path_drives_the_drawer_too() {
    use gumicord_platform::Application;
    let mut a = narrow();
    assert!(Application::drawer_drag_maybe(&a, 10.0));
    assert!(!Application::drawer_drag_maybe(&a, 300.0));
    assert!(Application::drawer_drag_start(&mut a));
    assert!(Application::drawer_drag_move(&mut a, 140.0, 280.0));
    assert!(Application::drawer_drag_end(&mut a, 0.0));
    assert_eq!(Application::drawer_slide(&a), 0.5);
    // An open drawer answers a close drag through the same path.
    assert!(a.open_drawer());
    settle_drawer(&mut a);
    assert!(a.chat.drawer_open);
    assert!(Application::drawer_close_drag_maybe(&a));
    assert!(Application::drawer_close_drag_start(&mut a));
}

/// Opening starts the sheet below and coasts it up over time.
#[test]
fn opening_coasts_the_sheet_in_over_time() {
    use std::time::{Duration, Instant};
    let mut a = narrow();
    let start = Instant::now();
    assert!(a.open_member_sheet());
    assert_eq!(a.sheet_slide, 0.0, "開いた瞬間にいる");
    assert!(a.advance_sheet(start + Duration::from_millis(110)));
    let mid = a.sheet_slide;
    assert!(mid > 0.0 && mid < 1.0, "途中にいない: {mid}");
    assert!(!a.advance_sheet(start + Duration::from_secs(1)));
    assert_eq!(a.sheet_slide, 1.0);
    assert!(a.chat.member_sheet_open, "着いたのに閉じた");
}

/// Closing coasts out and flips the flag only on landing.
#[test]
fn closing_coasts_the_sheet_out_and_lands_shut() {
    use std::time::{Duration, Instant};
    let mut a = narrow();
    assert!(a.open_member_sheet());
    settle_sheet(&mut a);
    assert!(a.close_member_sheet());
    assert!(a.chat.member_sheet_open, "閉じ始めに消えた");
    assert!(!a.advance_sheet(Instant::now() + Duration::from_secs(1)));
    assert!(!a.chat.member_sheet_open, "着いたのに残っている");
    assert_eq!(a.sheet_slide, 0.0);
}

/// The finger drives the sheet; letting go coasts to a side.
#[test]
fn the_finger_drives_the_sheet_and_letting_go_coasts() {
    // A slow release past halfway down finishes closing.
    let mut a = narrow();
    assert!(a.open_member_sheet());
    settle_sheet(&mut a);
    assert!(a.sheet_drag_start());
    assert!(a.sheet_drag, "掴んでいない");
    assert!(a.sheet_drag_move(300.0, 500.0));
    assert!(
        (a.sheet_slide - 0.4).abs() < 0.001,
        "付いてこない: {}",
        a.sheet_slide
    );
    assert!(a.sheet_drag_end(0.0));
    assert!(!a.sheet_drag, "離したのに掴んだまま");
    assert_eq!(a.sheet_target, 0.0, "半分過ぎたのに戻る");

    // A fast flick up swings back open from anywhere.
    let mut b = narrow();
    assert!(b.open_member_sheet());
    settle_sheet(&mut b);
    assert!(b.sheet_drag_start());
    assert!(b.sheet_drag_move(400.0, 500.0));
    assert!(b.sheet_drag_end(-1000.0));
    assert_eq!(b.sheet_target, 1.0, "払ったのに閉じる");

    // A fast flick down closes whatever the progress.
    let mut c = narrow();
    assert!(c.open_member_sheet());
    settle_sheet(&mut c);
    assert!(c.sheet_drag_start());
    assert!(c.sheet_drag_move(10.0, 500.0));
    assert!(c.sheet_drag_end(1000.0));
    assert_eq!(c.sheet_target, 0.0, "払ったのに開く");
}

/// The finger drives an open drawer shut; letting go picks a side.
#[test]
fn the_finger_drives_an_open_drawer_shut() {
    let mut a = narrow();
    assert!(a.open_drawer());
    settle_drawer(&mut a);
    // A slow drag left past halfway falls shut.
    assert!(a.drawer_close_drag_start());
    assert!(a.drawer_drag_move(-140.0, 280.0));
    assert_eq!(a.chat.drawer_slide, 0.5);
    assert!(a.drawer_drag_end(0.0));
    assert_eq!(a.chat.drawer_target, 0.0, "半分過ぎたのに戻る");

    // A small drag springs back open.
    let mut b = narrow();
    assert!(b.open_drawer());
    settle_drawer(&mut b);
    assert!(b.drawer_close_drag_start());
    assert!(b.drawer_drag_move(-28.0, 280.0));
    assert!(b.drawer_drag_end(0.0));
    assert_eq!(b.chat.drawer_target, 1.0, "少しで閉じる");
}

/// A coasting sheet wakes the loop until it lands.
#[test]
fn a_coasting_sheet_wakes_the_loop_until_it_lands() {
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let wake = a.next_frame_in().expect("寝てしまう");
    assert!(wake <= std::time::Duration::from_millis(16), "{wake:?}");
    settle_sheet(&mut a);
    // Landed and idle: nothing to wake for without the overlay tick.
    assert_eq!(a.next_frame_in(), None);
}

/// The trait path drives the sheet too, without recursing.
#[test]
fn the_trait_path_drives_the_sheet_too() {
    use gumicord_platform::Application;
    let mut a = narrow();
    assert!(a.open_member_sheet());
    settle_sheet(&mut a);
    assert!(Application::sheet_drag_maybe(&a));
    assert!(Application::sheet_drag_start(&mut a));
    assert!(Application::sheet_drag_move(&mut a, 150.0, 500.0));
    assert!(Application::sheet_drag_end(&mut a, 0.0));
    assert!((Application::sheet_slide(&a) - 0.7).abs() < 0.001);
}

/// Dismissing a menu sheet coasts out; choosing acts at once.
#[test]
fn dismissing_a_menu_sheet_coasts_out_while_choosing_acts_at_once() {
    let mut a = narrow_menu();
    assert_eq!(a.sheet_slide, 0.0, "開いた瞬間にいる");
    assert!(a.close_menu());
    assert!(a.floating.is_some(), "閉じ始めに消えた");
    settle_sheet(&mut a);
    assert!(a.floating.is_none(), "着いたのに残っている");

    // Choosing an item vanishes at once instead of coasting.
    let mut b = narrow_menu();
    settle_sheet(&mut b);
    b.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
        at: (0.0, 0.0),
        items: vec![crate::menu::Item::new(
            crate::menu::Action::MarkRead(1),
            "既読にする",
        )],
    }));
    let hits = [hit_of(NodeId::OverlayMenuItem, Some(Key::Index(0)))];
    assert!(b.pressed(&hits));
    assert!(b.floating.is_none(), "選んだのに残っている");
}

/// Opening a menu closes the member sheet: a single sheet rides the
/// slide channel.
#[test]
fn opening_a_menu_closes_the_member_sheet() {
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
    assert!(a.context_menu(std::slice::from_ref(&msg), (10.0, 20.0)));
    assert!(!a.chat.member_sheet_open, "面が残っている");
    assert!(matches!(a.floating, Some(crate::menu::Floating::Menu(_))));
}

/// An edge swipe opens the drawer only where the lists hide.
#[test]
fn edge_swipe_opens_the_drawer_when_narrow() {
    let mut a = narrow();
    assert!(a.swiped(&[], swipe(SwipeDir::Right, 10.0)));
    assert!(a.chat.drawer_open, "棚が開かない");

    let mut wide = app();
    wide.match_ctx = MatchContext::new(1400.0);
    assert!(!wide.swiped(&[], swipe(SwipeDir::Right, 10.0)));
    assert!(!wide.chat.drawer_open, "広いのに開いた");

    let mut mid = narrow();
    assert!(!mid.swiped(&[], swipe(SwipeDir::Right, 300.0)));
    assert!(!mid.chat.drawer_open, "端でないのに開いた");
}

/// The drawer holds both lists and navigates, then closes.
#[test]
fn drawer_selects_a_channel_then_closes() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let mut found = false;
    a.build_tree(Panes::One).walk(&mut |n, _| {
        found = found
            || n.id == NodeId::OverlayDrawer
            || n.id == NodeId::NavGuildList
            || n.id == NodeId::NavChannelList;
    });
    assert!(found, "棚の中身がない");

    // A real press inside the drawer carries the drawer's hit too.
    let drawer = hit_at(NodeId::OverlayDrawer, None, 0.0, 0.0, 280.0, 800.0);
    let item = hit_at(
        NodeId::NavChannelListItem,
        Some(Key::Id(10)),
        8.0,
        100.0,
        264.0,
        40.0,
    );
    assert!(a.pressed(&[item, drawer]));
    assert_eq!(a.chat.selected_channel, 10);
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "選んだのに閉じない");
}

/// A press behind the drawer dismisses it but never acts through it:
/// the drawer's reused lists share IDs with the chat behind, so only
/// hits inside the drawer's rectangle count.
#[test]
fn a_press_behind_the_drawer_dismisses_without_acting() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let channel = a.chat.selected_channel;
    // A channel row behind the drawer: the same ID, outside its
    // rectangle, and no drawer hit at all.
    let behind = hit_at(
        NodeId::NavChannelListItem,
        Some(Key::Id(20)),
        300.0,
        100.0,
        80.0,
        40.0,
    );
    assert!(a.pressed(&[behind]));
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "閉じていない");
    assert_eq!(a.chat.selected_channel, channel, "裏の一覧が動いた");
}

/// The drawer's own background is a dead zone: it neither acts nor
/// closes, so a finger missing a row does not lose the drawer.
#[test]
fn a_press_on_the_drawer_dead_zone_keeps_it_open() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let drawer = hit_at(NodeId::OverlayDrawer, None, 0.0, 0.0, 280.0, 800.0);
    assert!(!a.pressed(&[drawer]));
    assert!(a.chat.drawer_open, "隙間で閉じた");
}

/// The drawer drags a dim scrim over the chat behind it: what looks
/// dimmed is also untouchable.
#[test]
fn the_drawer_drags_a_scrim_with_it() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let mut ids = Vec::new();
    a.build_tree(Panes::One)
        .walk(&mut |n, _| ids.push((n.id, n.key.clone())));
    let scrim = ids
        .iter()
        .position(|(id, k)| *id == NodeId::OverlayScrim && *k == Some(Key::Slot("dim")))
        .expect("覆いがない");
    let drawer = ids
        .iter()
        .position(|(id, _)| *id == NodeId::OverlayDrawer)
        .expect("棚がない");
    assert!(scrim < drawer, "覆いが棚より手前にある");

    let b = narrow();
    let mut closed = Vec::new();
    b.build_tree(Panes::One).walk(&mut |n, _| closed.push(n.id));
    assert!(
        !closed.contains(&NodeId::OverlayScrim),
        "閉じているのに覆いがある"
    );
}

/// Tapping outside the drawer dismisses it without navigating.
#[test]
fn tapping_outside_the_drawer_dismisses_it() {
    let mut a = narrow();
    assert!(a.open_drawer());
    let channel = a.chat.selected_channel;
    assert!(a.pressed(&[hit_of(NodeId::ChatMessage, Some(Key::Id(1)))]));
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "閉じていない");
    assert_eq!(a.chat.selected_channel, channel, "下のチャンネルへ移動した");
}

/// Any press outside a field drops focus, or phones keep the keyboard
/// with no way to dismiss it.
#[test]
fn pressing_outside_a_field_releases_focus() {
    let mut a = app();
    a.chat.input_focused = true;
    a.login_view.field = Some(LoginField::Email);
    assert!(a.pressed(&[hit_of(NodeId::ChatMessage, Some(Key::Id(1)))]));
    assert!(!a.chat.input_focused, "入力欄に焦点が残っている");
    assert!(a.login_view.field.is_none(), "ログイン欄に焦点が残っている");
}

/// A message menu must not hide behind the keyboard.
#[test]
fn opening_a_message_menu_releases_focus() {
    let mut a = app();
    a.chat.input_focused = true;
    let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
    assert!(a.context_menu(std::slice::from_ref(&msg), (10.0, 20.0)));
    assert!(a.floating.is_some(), "メニューが開かなかった");
    assert!(
        !a.chat.input_focused,
        "メニューの裏にキーボードが残っている"
    );
}

/// The field menu acts on the focused field, so it keeps focus.
#[test]
fn opening_a_field_menu_keeps_focus() {
    let mut a = app();
    a.chat.input.insert("abc");
    let hits = [hit_of(NodeId::ChatInputField, None)];
    assert!(a.context_menu(&hits, (0.0, 0.0)));
    assert!(a.floating.is_some(), "メニューが開かなかった");
    assert!(a.chat.input_focused, "欄のメニューが欄を失った");
}

/// One tap outside the menu dismisses both it and the keyboard.
#[test]
fn pressing_outside_an_open_menu_releases_focus() {
    let mut a = with_menu();
    a.chat.input_focused = true;
    let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
    assert!(a.pressed(std::slice::from_ref(&msg)));
    settle_sheet(&mut a);
    assert!(a.floating.is_none(), "メニューが閉じていない");
    assert!(!a.chat.input_focused, "キーボードが閉じていない");
}

/// Opening the drawer drops focus; it holds no text fields.
#[test]
fn opening_the_drawer_releases_focus() {
    let mut a = narrow();
    a.chat.input_focused = true;
    assert!(a.open_drawer());
    assert!(!a.chat.input_focused, "棚の裏にキーボードが残っている");
}

/// Tapping through the drawer drops focus, not just the drawer.
#[test]
fn tapping_outside_the_drawer_releases_focus() {
    let mut a = narrow();
    assert!(a.open_drawer());
    a.chat.input_focused = true;
    assert!(a.pressed(&[hit_of(NodeId::ChatMessage, Some(Key::Id(1)))]));
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "閉じていない");
    assert!(!a.chat.input_focused, "キーボードが閉じていない");
}

/// Opening settings drops focus; the screen holds no text fields.
#[test]
fn opening_settings_releases_focus() {
    let mut a = app();
    a.chat.input_focused = true;
    assert!(a.open_settings());
    assert!(a.settings.open, "開かなかった");
    assert!(!a.chat.input_focused, "設定の裏にキーボードが残っている");
}

/// Pressing a settings row drops focus too.
#[test]
fn pressing_a_settings_row_releases_focus() {
    let mut a = app();
    assert!(a.open_settings());
    a.chat.input_focused = true;
    assert!(press_menu(&mut a, 0));
    assert!(!a.chat.input_focused, "キーボードが閉じていない");
}

/// Escape closes the drawer and the sheet, after menus and settings.
#[test]
fn escape_closes_drawer_and_sheet() {
    let mut a = narrow();
    assert!(a.open_drawer());
    assert!(a.cancel_input());
    settle_drawer(&mut a);
    assert!(!a.chat.drawer_open, "Esc で閉じない");

    assert!(a.open_member_sheet());
    assert!(a.cancel_input());
    settle_sheet(&mut a);
    assert!(!a.chat.member_sheet_open, "Esc で閉じない");
}

/// The member button opens the sheet only where the pane hides.
#[test]
fn members_button_opens_the_sheet_when_narrow() {
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let mut sheet = false;
    let mut list = false;
    a.build_tree(Panes::One).walk(&mut |n, _| {
        sheet = sheet || n.id == NodeId::OverlaySheet;
        list = list || n.id == NodeId::NavMemberListSheet;
    });
    assert!(sheet && list, "面か一覧が出ていない");

    // Tapping a row closes the sheet; there is no profile view yet.
    assert!(a.pressed(&[hit_of(NodeId::NavMemberListItem, Some(Key::Id(5)))]));
    settle_sheet(&mut a);
    assert!(!a.chat.member_sheet_open, "閉じていない");

    let mut wide = app();
    wide.match_ctx = MatchContext::new(1400.0);
    assert!(!wide.open_member_sheet(), "広いのに開いた");
}

/// The header carries the member button only while the pane hides.
#[test]
fn header_button_appears_only_when_narrow() {
    fn has_button(a: &Gumicord, panes: Panes) -> bool {
        let mut found = false;
        a.build_tree(panes).walk(&mut |n, _| {
            found = found
                || (n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot(MEMBERS_OPEN)));
        });
        found
    }
    assert!(has_button(&narrow(), Panes::One));
    let mut wide = app();
    wide.match_ctx = MatchContext::new(1400.0);
    assert!(!has_button(&wide, Panes::Four));
}

/// The members mark names a real icon; unknown names draw nothing.
#[test]
fn members_icon_exists() {
    assert!(
        gumicord_render::icon::lookup(MEMBERS_ICON).is_some(),
        "人形札の絵がない"
    );
    assert!(
        gumicord_render::icon::lookup(BACK_ICON).is_some(),
        "戻る札の絵がない"
    );
}

/// The drawer stands at the left edge, narrower than the window.
#[test]
fn drawer_stands_at_the_left_edge() {
    let (w, h) = (400.0, 800.0);
    let mut a = narrow();
    assert!(a.open_drawer());
    settle_drawer(&mut a);
    let cx = gumicord_platform::FrameCx {
        viewport: gumicord_render::Size::new(w, h),
        scale: 1.0,
    };
    let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);
    let drawer = placed
        .iter()
        .find(|(id, _)| *id == NodeId::OverlayDrawer)
        .map(|(_, r)| *r)
        .expect("棚が置かれていない");
    assert!(drawer.x.abs() < 1.0, "左端にいない {drawer:?}");
    assert!(drawer.w < w, "全画面を覆っている {drawer:?}");
}

/// The sheet spans the width, rises to ~70% at most, and sits at
/// the bottom.
#[test]
fn member_sheet_spans_and_caps() {
    let (w, h) = (400.0, 800.0);
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let cx = gumicord_platform::FrameCx {
        viewport: gumicord_render::Size::new(w, h),
        scale: 1.0,
    };
    let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);
    let sheet = placed
        .iter()
        .find(|(id, _)| *id == NodeId::OverlaySheet)
        .map(|(_, r)| *r)
        .expect("面が置かれていない");
    assert!((sheet.w - w).abs() < 1.0, "横いっぱいでない {sheet:?}");
    assert!(sheet.h <= h * 0.7 + 1.0, "高すぎる {sheet:?}");
    assert!(
        (sheet.y + sheet.h - h).abs() < 1.0,
        "下に付いていない {sheet:?}"
    );
    let list = placed
        .iter()
        .find(|(id, _)| *id == NodeId::NavMemberListSheet)
        .map(|(_, r)| *r)
        .expect("一覧が出ていない");
    assert!(
        (list.w - sheet.w).abs() < 1.0,
        "一覧が面を埋めていない {list:?} {sheet:?}"
    );
}

/// The sheet grabber sits centred at the top, not stuck to the left edge.
#[test]
fn the_sheet_handle_sits_centred() {
    let (w, h) = (400.0, 800.0);
    let mut a = narrow();
    assert!(a.open_member_sheet());
    let cx = gumicord_platform::FrameCx {
        viewport: gumicord_render::Size::new(w, h),
        scale: 1.0,
    };
    let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);
    let sheet = placed
        .iter()
        .find(|(id, _)| *id == NodeId::OverlaySheet)
        .map(|(_, r)| *r)
        .expect("面が置かれていない");
    let pill = placed
        .iter()
        .filter(|(id, r)| {
            *id == NodeId::LayoutRow
                && (r.w - 36.0).abs() < 1.0
                && r.y >= sheet.y
                && r.y < sheet.y + 40.0
        })
        .map(|(_, r)| *r)
        .next()
        .expect("掴みしろがない");
    let centred = sheet.x + (sheet.w - pill.w) / 2.0;
    assert!(
        (pill.x - centred).abs() < 1.0,
        "中央にいない {pill:?} {sheet:?}"
    );
}

/// The back button opens the drawer from a press too.
#[test]
fn back_button_opens_the_drawer() {
    let mut a = narrow();
    let mut found = false;
    a.build_tree(Panes::One).walk(&mut |n, _| {
        found = found || (n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot(BACK_OPEN)));
    });
    assert!(found, "戻る札が出ていない");
    assert!(a.pressed(&[hit_of(NodeId::PrimitiveButton, Some(Key::Slot(BACK_OPEN)))]));
    assert!(a.chat.drawer_open, "戻る札で開かない");
}

// ═══════════════════════════════════════════════════════════════
//  Context menus

#[test]
fn right_clicking_a_message_opens_the_menu() {
    let a = with_menu();
    assert!(a.floating.is_some());
    assert_eq!(
        a.floating.as_ref().and_then(|f| match f {
            crate::menu::Floating::Menu(m) => Some(m.at),
            _ => None,
        }),
        Some((10.0, 20.0))
    );
}

/// A covered run opens alone and closes again on the second press; under
/// an open menu it declines exactly like a link does.
#[test]
fn a_spoiler_press_toggles_and_declines_while_something_floats() {
    let mut a = with_menu();
    assert!(!a.spoiler_pressed(1, 0));
    assert!(!a.chat.reveals.is_open(1, 0), "断ったのに開いている");

    // Toggle: open once, then cover again.
    let mut b = app();
    assert!(b.spoiler_pressed(5, 2));
    assert!(b.chat.reveals.is_open(5, 2));
    assert!(b.spoiler_pressed(5, 2));
    assert!(!b.chat.reveals.is_open(5, 2), "もう一度押しても閉じない");

    // The message-level reveal still counts as open for every run.
    b.chat.reveals.messages.insert(5);
    assert!(b.spoiler_pressed(5, 2));
    assert!(
        b.chat.reveals.is_open(5, 2),
        "メッセージ全体が開いているのに閉じた"
    );
}

/// Signing out is destructive and hard to reverse without a phone, so it
/// goes through the same dialog as deleting.
#[test]
fn logging_out_asks_first() {
    let mut a = app();
    a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
        at: (0.0, 0.0),
        items: vec![crate::menu::Item::new(
            crate::menu::Action::LogOut,
            "ログアウト",
        )],
    }));
    press_menu(&mut a, 0);
    assert!(is_confirm(&a), "no confirmation appeared");
}

/// The dialog has to say the phone is needed, since password login does
/// not exist yet.
#[test]
fn the_logout_dialog_says_a_phone_is_needed() {
    let a = app();
    let c = a
        .needs_confirming(
            &crate::menu::Floating::Menu(crate::menu::Menu {
                at: (0.0, 0.0),
                items: Vec::new(),
            }),
            &crate::menu::Action::LogOut,
        )
        .expect("log out should be confirmed");
    assert!(c.danger);
    assert!(c.body.contains("QR"), "does not mention the QR: {}", c.body);
}

/// Only offered while signed in; there is nothing to sign out of otherwise.
#[test]
fn the_user_menu_is_empty_when_signed_out() {
    assert!(app().user_menu().is_empty());
}

/// Demo mode has no runtime and nothing to sign out of.
#[test]
fn signing_out_without_a_runtime_does_nothing() {
    let mut a = app();
    assert!(!a.sign_out());
}

// ═══════════════════════════════════════════════════════════════
//  Time-dependent display

/// With no relative timestamp there is nothing to wake for, and a
/// deadline would spin for no change.
#[test]
fn nothing_relative_means_no_wake_up() {
    let mut a = app();
    built(&mut a);
    assert_eq!(a.next_frame_in(), None);
}

/// Otherwise "just now" stays on an open screen for hours.
#[test]
fn a_relative_timestamp_asks_for_a_later_frame() {
    let mut a = app();
    // Relative to the real clock; a fixed timestamp would drift into
    // "years ago".
    let at = gumicord_platform::now_unix() - 90;
    let channel = ChannelId::from(a.chat.selected_channel);
    a.live.store_mut().set_backlog(
        channel,
        vec![gumicord_model::Message {
            id: MessageId::from(9_999u64),
            channel_id: channel,
            guild_id: None,
            author: gumicord_model::User {
                id: UserId::from(7u64),
                username: "nenneko".to_owned(),
                global_name: Some("ねんねこ".to_owned()),
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: format!("<t:{at}:R>"),
            timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }],
    );
    built(&mut a);

    let d = a.next_frame_in().expect("起き直しを頼んでいない");
    assert!(
        d.as_secs() >= 1 && d.as_secs() <= 60,
        "分の切れ目のはずが {d:?}"
    );
}

// ═══════════════════════════════════════════════════════════════
//  Confirming before deleting

/// Matching on the raw string would notify someone for writing about a
/// mention inside code.
#[test]
fn a_mention_inside_code_is_not_a_mention() {
    let me = Some(UserId::from(1));
    let call =
        |src: &str| crate::pages::chat::rows::calls_me(&gumicord_markdown::parse(src), me, None);

    assert!(call("やあ <@1>"));
    assert!(!call("`<@1>` と書くと呼べる"));
    assert!(!call(
        "```
<@1>
```"
    ));
    // A different person is not us.
    assert!(!call("やあ <@2>"));
}

/// Watching only `@everyone` misses being called by role.
#[test]
fn a_mention_of_our_own_role_counts() {
    let me = Some(UserId::from(1));
    let roles = [RoleId::from(9)];
    let call = |src: &str, r: Option<&[RoleId]>| {
        crate::pages::chat::rows::calls_me(&gumicord_markdown::parse(src), me, r)
    };

    assert!(call("<@&9> 集合", Some(&roles)));
    assert!(!call("<@&8> 集合", Some(&roles)));
    assert!(!call("<@&9> 集合", None));
    assert!(call("@everyone", None));
    assert!(call("@here", None));
    // A channel reference is not a mention.
    assert!(!call("<#9> を見て", Some(&roles)));
}

/// Mentions inside quotes and lists count.
#[test]
fn a_nested_mention_is_found() {
    let me = Some(UserId::from(1));
    let call =
        |src: &str| crate::pages::chat::rows::calls_me(&gumicord_markdown::parse(src), me, None);
    assert!(call("> やあ <@1>"));
    assert!(call("- やあ <@1>"));
    assert!(call("# やあ <@1>"));
}

/// The tree builds and reflects the selection.
#[test]
fn the_tree_reflects_the_selection() {
    let mut a = app();
    // Demo rows are gone; the selection shows through live data.
    a.live
        .store_mut()
        .replace_guilds(vec![gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![
                gumicord_model::Channel {
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
                },
                gumicord_model::Channel {
                    id: 12u64.into(),
                    kind: gumicord_model::ChannelKind::GuildText,
                    name: Some("ざつだん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 1,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
            ],
            roles: Vec::new(),
        }]);
    a.chat.selected_guild = 1;
    a.chat.selected_channel = 12;
    let tree = a.build_tree(Panes::Three);

    let mut selected = Vec::new();
    tree.walk(&mut |n, _| {
        if n.id == NodeId::NavChannelListItem && n.states.contains(State::Selected) {
            selected.push(n.key.clone());
        }
    });
    assert_eq!(selected, vec![Some(Key::Id(12))]);
}

/// A press that changes nothing asks for no redraw.
#[test]
fn pressing_the_current_channel_changes_nothing() {
    let mut a = app();
    let hit = Hit {
        id: NodeId::NavChannelListItem,
        key: Some(Key::Id(a.chat.selected_channel)),
        rect: gumicord_render::Rect::ZERO,
        clip: None,
    };
    assert!(!a.pressed(std::slice::from_ref(&hit)));
}

#[cfg(test)]
mod input_tests {
    use gumicord_uitree::{Content, Editable};

    use super::*;

    fn cx() -> FrameCx {
        FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        }
    }

    fn field(tree: &UiNode) -> Editable {
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatInputField
                && let Content::Editable(e) = &n.content
            {
                found = Some(e.clone());
            }
        });
        found.expect("入力欄が見つからない")
    }

    /// Input needs focus.
    #[test]
    fn input_only_reaches_a_focused_field() {
        let mut a = Gumicord::demo();
        assert!(a.focused_document().is_none());

        a.chat.input_focused = true;
        assert!(a.focused_document().is_some());
    }

    /// The preedit range reaches the tree, which is what draws the underline.
    #[test]
    fn a_composition_reaches_the_tree() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;

        let doc = a.focused_document().unwrap();
        doc.insert("送信: ");
        doc.set_composition("にほんご", None);

        let f = field(&a.build(&cx()));
        assert_eq!(f.text, "送信: にほんご");
        assert_eq!(
            f.composing,
            Some("送信: ".len().."送信: にほんご".len()),
            "変換中の範囲が伝わっていない"
        );
        assert_eq!(f.caret, f.text.len());
    }

    /// Empty shows the placeholder and no preedit marks.
    #[test]
    fn an_empty_field_shows_only_its_placeholder() {
        let mut a = Gumicord::demo();
        let f = field(&a.build(&cx()));
        assert!(f.text.is_empty());
        assert!(f.placeholder.contains("メッセージを送信"));
        assert!(f.composing.is_none());
    }

    /// Enter clears the field. Without live data there is nowhere to
    /// send, so nothing is appended.
    #[test]
    fn submitting_clears_the_field() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;
        a.focused_document().unwrap().insert("こんにちは");

        assert!(a.submit());
        assert!(
            field(&a.build(&cx())).text.is_empty(),
            "入力欄が空になっていない"
        );
    }

    /// Whitespace alone is not sent.
    #[test]
    fn whitespace_is_not_submitted() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;
        a.focused_document().unwrap().insert("   ");
        assert!(!a.submit());
    }

    /// Escape removes focus. During composition it cancels instead, and that
    /// branch belongs to the platform layer.
    #[test]
    fn escape_leaves_the_field() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;
        assert!(a.cancel_input());
        assert!(!a.chat.input_focused);
        assert!(!a.cancel_input(), "既に外れていれば何も起きない");
    }

    /// Shift+Enter is a newline, not a send.
    #[test]
    fn shift_enter_inserts_a_newline_instead_of_sending() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;
        a.focused_document().unwrap().insert("one");
        assert!(a.shift_enter());
        assert_eq!(a.chat.input.text(), "one\n");
        assert_eq!(a.chat.composing, Composing::New);
    }

    /// Login fields stay single-line: Shift+Enter is unhandled there, so
    /// the caller falls through to submitting.
    #[test]
    fn shift_enter_on_a_login_field_is_unhandled() {
        let mut a = Gumicord::demo();
        a.login_view.field = Some(LoginField::Email);
        a.chat.input_focused = true;
        assert!(!a.shift_enter());
        assert!(a.chat.input.text().is_empty());
    }

    /// A multiline draft sends whole.
    #[test]
    fn a_multiline_draft_sends_whole() {
        let mut a = Gumicord::demo();
        a.chat.input_focused = true;
        a.focused_document().unwrap().insert("one\ntwo");
        assert!(a.submit());
        assert!(
            field(&a.build(&cx())).text.is_empty(),
            "入力欄が空になっていない"
        );
    }

    /// The field grows with its lines.
    ///
    /// ASCII only: the CI runner may have no Japanese font.
    #[test]
    fn the_field_grows_with_its_lines() {
        fn height(text: &str) -> f32 {
            let mut a = Gumicord::demo();
            a.chat.input_focused = true;
            a.focused_document().unwrap().insert(text);
            let cx = cx();
            let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);
            placed
                .iter()
                .find(|(id, _)| *id == NodeId::ChatInputField)
                .map(|(_, r)| r.h)
                .expect("入力欄が置かれていない")
        }
        let one = height("one");
        let three = height("one\ntwo\nthree");
        assert!(
            three > one + 1.0,
            "3 行が 1 行と変わらない ({three} <= {one})"
        );
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    /// Nobody typing shows nothing.
    #[test]
    fn nobody_typing_says_nothing() {
        assert_eq!(typing_line(&[]), "");
    }

    #[test]
    fn one_and_two_and_three_are_named() {
        assert_eq!(typing_line(&["あ"]), "  あ が入力中…");
        assert_eq!(typing_line(&["あ", "い"]), "  あ と い が入力中…");
        assert_eq!(typing_line(&["あ", "い", "う"]), "  あ、い、う が入力中…");
    }

    /// Fits on one line even on a busy server.
    #[test]
    fn a_crowd_is_summarised() {
        let many = ["あ", "い", "う", "え", "お", "か"];
        assert_eq!(typing_line(&many), "  あ、い ほか 4 人が入力中…");
    }
}

#[cfg(test)]
mod user_panel_tests {
    use super::*;

    fn names(node: &UiNode) -> Vec<NodeId> {
        fn walk(n: &UiNode, out: &mut Vec<NodeId>) {
            out.push(n.id);
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(node, &mut out);
        out
    }

    /// Nothing to show while signed out.
    #[test]
    fn there_is_no_panel_before_logging_in() {
        let a = Gumicord::demo();
        assert!(a.user_panel().is_none());
        let side = a.sidebar(Panes::Three).unwrap();
        assert!(!names(&side).contains(&NodeId::NavUserPanel));
    }

    /// The user panel spans the guild list too; inside the channel list it
    /// would only be as wide as that.
    #[test]
    fn the_panel_spans_both_lists() {
        let a = Gumicord::demo();
        let side = a.sidebar(Panes::Three).expect("3 ペインなら出る");

        assert_eq!(side.id, NodeId::NavSidebar);
        assert_eq!(side.children[0].id, NodeId::NavSidebarLists);
        // The lists are grouped; the panel sits outside them.
        let inside = names(&side.children[0]);
        assert!(inside.contains(&NodeId::NavGuildList));
        assert!(inside.contains(&NodeId::NavChannelList));
        assert!(!inside.contains(&NodeId::NavUserPanel));

        // Growing here would take width from chat.
        assert_eq!(gumicord_render::intrinsic(NodeId::NavSidebar).grow, 0.0);
    }

    /// Only the list under the pointer gets a scrollbar.
    #[test]
    fn only_the_list_under_the_pointer_has_a_scrollbar() {
        let mut a = Gumicord::demo();

        // Nowhere means none of them.
        assert!(!names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));

        a.hovered_scroll = Some(NodeId::NavGuildList);
        assert!(names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        // Not on the neighbouring list.
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));
    }

    /// The innermost scrollable wins.
    #[test]
    fn the_innermost_scroll_region_wins() {
        let mut a = Gumicord::demo();
        let hit = |id| Hit {
            id,
            key: None,
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        };

        // Front to back: item, inner scrollable, outer container.
        a.hover_changed(&[
            hit(NodeId::NavChannelListItem),
            hit(NodeId::LayoutScroll),
            hit(NodeId::NavChannelList),
        ]);
        assert_eq!(a.hovered_scroll, Some(NodeId::LayoutScroll));

        a.hover_changed(&[]);
        assert_eq!(a.hovered_scroll, None);
    }

    /// At the narrowest width the lists go, and the panel with them.
    #[test]
    fn one_pane_has_no_sidebar_at_all() {
        let a = Gumicord::demo();
        assert!(a.sidebar(Panes::One).is_none());
    }

    /// One scroll region would carry the header and the panel off screen.
    #[test]
    fn only_the_list_scrolls() {
        let a = Gumicord::demo();
        let pane = a.channel_list();

        assert_eq!(pane.id, NodeId::NavChannelList);
        assert!(
            !gumicord_render::intrinsic(NodeId::NavChannelList).scroll,
            "外側が巻いてしまっている"
        );
        assert!(
            pane.children.iter().any(|c| c.id == NodeId::LayoutScroll),
            "巻く領域が中に無い"
        );
        assert_eq!(pane.children[0].id, NodeId::NavChannelListHeader);
    }

    #[test]
    fn user_menu_shows_account_options_and_masks_tokens() {
        let mut a = Gumicord::demo();
        let me = gumicord_model::CurrentUser {
            user: gumicord_model::User {
                id: UserId::from(1234567890u64),
                username: "Alice".to_owned(),
                discriminator: "0".to_owned(),
                global_name: Some("Alice".to_owned()),
                avatar_hash: None,
                bot: false,
            },
            email: None,
            verified: false,
            mfa_enabled: false,
        };
        let client = gumicord_rest::RestClient::anonymous().unwrap();
        let token = gumicord_model::Token::new("super_secret_token");
        a.login
            .set_logged_in(session::LoggedIn { me, client, token });

        let menu = a.user_menu();
        assert!(menu.iter().any(|it| it.label == "ID をコピー"));
        assert!(menu.iter().any(|it| it.label == "アカウントを追加"));
        assert!(menu.iter().any(|it| it.label == "ログアウト"));

        // Secret tokens must never appear in menu labels.
        for item in &menu {
            assert!(!item.label.contains("super_secret_token"));
        }
    }
}

#[cfg(test)]
mod member_tests {
    use super::*;
    use gumicord_model::{Member, Message, MessageId, User, UserId};

    fn message(nick: Option<&str>, member_avatar: Option<&str>) -> Message {
        Message {
            id: MessageId::from(100u64),
            channel_id: ChannelId::from(10u64),
            guild_id: None,
            author: User {
                id: UserId::from(7u64),
                username: "nenneko".to_owned(),
                global_name: Some("ねんねこ".to_owned()),
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: "こんにちは".to_owned(),
            timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: Some(Member {
                nick: nick.map(|s| s.to_owned()),
                avatar_hash: member_avatar.map(|s| s.to_owned()),
                roles: Vec::new(),
                joined_at: None,
                user: None,
            }),
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }
    }

    fn app(m: Message) -> Gumicord {
        let mut a = Gumicord::demo();
        // Without a guild this stays demo mode.
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
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 10;
        a
    }

    /// The per-guild name wins.
    #[test]
    fn a_nickname_wins_over_the_global_name() {
        let a = app(message(Some("ねこ"), None));
        assert_eq!(a.message_rows()[0].author, "ねこ");

        let a = app(message(None, None));
        assert_eq!(a.message_rows()[0].author, "ねんねこ");
    }

    /// Parsing once per content: repeats reuse the entry, an edit misses
    /// and re-parses instead of showing the old body.
    #[test]
    fn parsed_bodies_are_remembered_until_edited() {
        let a = app(message(None, None));
        let first = a.parsed_blocks(1, "hello **world**");
        let second = a.parsed_blocks(1, "hello **world**");
        assert_eq!(first, second);
        assert_eq!(a.blocks_cache.borrow().len(), 1);
        let edited = a.parsed_blocks(1, "hello edited");
        assert_ne!(edited, first);
        assert_eq!(a.blocks_cache.borrow().len(), 1);
    }

    fn reply_text(a: &Gumicord, row: &MessageRow) -> (Option<String>, bool) {
        let mut text = None;
        let mut avatar = false;
        a.message(row, false).walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageReplyRef {
                for c in &n.children {
                    if c.id == NodeId::PrimitiveText {
                        text = c.content.as_text().map(str::to_owned);
                    }
                    avatar = avatar || c.id == NodeId::ChatMessageReplyRefAvatar;
                }
            }
        });
        (text, avatar)
    }

    /// A reply shows who and what it answers, on the tree too.
    #[test]
    fn a_reply_shows_its_source() {
        let mut m = message(None, None);
        m.referenced_message = Some(Box::new(message(None, None)));
        let a = app(m);
        let rows = a.message_rows();
        let reply = rows[0].reply.as_ref().expect("返信元がない");
        assert_eq!(reply.author, "ねんねこ");
        assert_eq!(reply.snippet, "こんにちは");
        assert!(reply.avatar.is_some(), "小アイコンがない");
        let (text, avatar) = reply_text(&a, &rows[0]);
        assert_eq!(text.as_deref(), Some("ねんねこ: こんにちは"));
        assert!(avatar, "木に小アイコンがない");
    }

    /// No reference, no row.
    #[test]
    fn a_plain_message_has_no_reply_ref() {
        let a = app(message(None, None));
        let rows = a.message_rows();
        assert!(rows[0].reply.is_none());
        let mut found = false;
        a.message(&rows[0], false).walk(&mut |n, _| {
            found = found || n.id == NodeId::ChatMessageReplyRef;
        });
        assert!(!found, "参照表示が出ている");
    }

    /// Pressing a reply reference queues a jump to the answered message.
    /// An unloaded target says so instead of stranding the reader.
    #[test]
    fn pressing_a_reply_queues_a_jump_to_its_source() {
        let mut target = message(None, None);
        target.id = MessageId::from(99u64);
        let mut m = message(None, None);
        m.referenced_message = Some(Box::new(target.clone()));
        let mut a = app(message(None, None));
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![target, m]);

        let press = |a: &mut Gumicord, id: u64| {
            a.pressed(&[Hit {
                id: NodeId::ChatMessageReplyRef,
                key: Some(Key::Id(id)),
                rect: gumicord_render::Rect::ZERO,
                clip: None,
            }])
        };
        assert!(press(&mut a, 99));
        assert_eq!(a.chat.pending_reveal, Some(99));
        assert_eq!(a.chat.a11y_message, Some(99), "読み上げが付いていかない");
        let req = a.take_reveal().expect("ジャンプが出ない");
        assert_eq!(req.region, NodeId::ChatMessageList);
        assert_eq!(req.id, NodeId::ChatMessage);
        assert_eq!(req.key, Some(Key::Id(99)));
        assert!(a.take_reveal().is_none(), "ジャンプが繰り返す");

        assert!(press(&mut a, 77));
        assert!(a.chat.pending_reveal.is_none(), "無い所へ飛ぼうとした");
        assert!(!a.toasts.is_empty(), "黙って失敗した");
    }

    /// An unloaded target fetches around instead of stranding; without a
    /// runtime it says so at once.
    #[test]
    fn an_unloaded_jump_fetches_or_says_so() {
        let mut a = app(message(None, None));
        assert!(a.jump_to_message(77));
        assert!(a.chat.pending_jump.is_none(), "飛べないのに待っている");
        assert!(!a.toasts.is_empty(), "黙って失敗した");
    }

    /// A fetched window settles into a reveal; moving on drops it.
    #[test]
    fn a_fetched_window_settles_into_a_reveal() {
        let mut target = message(None, None);
        target.id = MessageId::from(99u64);
        let mut m = message(None, None);
        m.referenced_message = Some(Box::new(target.clone()));
        let mut a = app(message(None, None));
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![target, m]);

        a.chat.pending_jump = Some((ChannelId::from(10u64), 99));
        a.settle_jump();
        assert_eq!(a.chat.pending_reveal, Some(99));
        assert!(a.chat.pending_jump.is_none());
        assert!(a.take_reveal().is_some());

        a.chat.pending_jump = Some((ChannelId::from(11u64), 99));
        a.settle_jump();
        assert!(a.chat.pending_jump.is_none(), "去った先へ飛ぼうとした");
        assert!(a.take_reveal().is_none());
    }

    /// Long sources truncate to one line.
    #[test]
    fn a_long_source_truncates_to_one_line() {
        let mut m = message(None, None);
        let mut r = message(None, None);
        r.content = "あ".repeat(200) + "\n二行目";
        m.referenced_message = Some(Box::new(r));
        let a = app(m);
        let rows = a.message_rows();
        let snippet = &rows[0].reply.as_ref().expect("返信元がない").snippet;
        assert_eq!(
            snippet.chars().count(),
            crate::pages::chat::rows::REPLY_SNIPPET_LEN + 1
        );
        assert!(snippet.ends_with('…'), "{snippet}");
        assert!(!snippet.contains('\n'), "{snippet}");
    }

    /// Avatars are per guild too; the guild appears in the URL.
    #[test]
    fn a_guild_avatar_wins_over_the_global_one() {
        let a = app(message(None, Some("xyz")));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(
            url.starts_with("https://cdn.discordapp.com/guilds/1/users/7/avatars/xyz.png"),
            "{url}"
        );

        // No override and no avatar means the default one.
        let a = app(message(None, None));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(url.contains("/embed/avatars/"), "{url}");
    }

    /// Colouring the member list but not the author line makes one person
    /// look like two.
    #[test]
    fn the_author_name_carries_the_role_colour() {
        let mut a = app(message(None, None));
        a.live.store_mut().upsert_guild(gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![gumicord_model::Role {
                id: 55u64.into(),
                name: "管理者".to_owned(),
                position: 3,
                hoist: true,
                color: Some(0x00e0_5260),
            }],
        });

        // Swap in a message from someone with that role.
        let mut m = message(None, None);
        m.member.as_mut().expect("居る").roles = vec![55u64.into()];
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        assert_eq!(a.message_rows()[0].tint, Some(0x00e0_5260));

        // And it reaches the tree.
        let tree = a.chat_view();
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageHeaderAuthor {
                found = n.tint;
            }
        });
        assert_eq!(found, Some(Color::from_rgb(0x00e0_5260)));
    }

    /// REST messages carry no `member`. Reading only the message left a
    /// freshly opened channel with no nicknames, avatars or colours until one
    /// new message arrived and coloured just that row.
    #[test]
    fn a_message_without_a_member_falls_back_to_what_we_have_seen() {
        let mut a = app(message(None, None));
        a.live.store_mut().upsert_guild(gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![gumicord_model::Role {
                id: 55u64.into(),
                name: "管理者".to_owned(),
                position: 3,
                hoist: true,
                color: Some(0x00e0_5260),
            }],
        });
        // A member seen in the list or in earlier messages.
        a.live.store_mut().remember_member(
            1u64.into(),
            7u64.into(),
            Member {
                nick: Some("ねこ".to_owned()),
                avatar_hash: None,
                roles: vec![55u64.into()],
                joined_at: None,
                user: None,
            },
        );

        // From REST, so no `member`.
        let mut m = message(None, None);
        m.member = None;
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        let row = &a.message_rows()[0];
        assert_eq!(row.author, "ねこ", "呼び名も出る");
        assert_eq!(row.tint, Some(0x00e0_5260), "役職の色も出る");
    }

    fn stamped(id: u64, user: u64, name: &str, timestamp: &str) -> Message {
        let mut m = message(None, None);
        m.id = MessageId::from(id);
        m.author.id = UserId::from(user);
        m.author.username = name.to_owned();
        m.author.global_name = None;
        m.timestamp = timestamp.to_owned();
        m.member = None;
        m
    }

    fn backlog(a: &mut Gumicord, messages: Vec<Message>) {
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), messages);
    }

    /// Consecutive messages from one author share a header; the tree marks
    /// every message after the first grouped.
    #[test]
    fn close_messages_from_one_author_share_a_header() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-03T12:06:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_eq!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let (mut dividers, mut lines, mut grouped) = (0, 0, Vec::new());
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageListDayDivider {
                dividers += 1;
            }
            if n.key == Some(Key::Slot("day_divider_line")) {
                lines += 1;
            }
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(dividers, 1, "one day, one divider");
        assert_eq!(lines, 2, "a line reaches each side");
        assert_eq!(grouped, vec![false, true]);
    }

    /// A new day breaks the run and draws its divider, even minutes apart.
    /// A 26-hour gap always spans a local midnight, on any machine.
    #[test]
    fn a_new_day_breaks_the_run_and_draws_its_divider() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-04T14:00:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_ne!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let (mut dividers, mut lines, mut grouped) = (0, 0, Vec::new());
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageListDayDivider {
                dividers += 1;
            }
            if n.key == Some(Key::Slot("day_divider_line")) {
                lines += 1;
            }
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(dividers, 2, "one divider per day, starting with the first");
        assert_eq!(lines, 4, "a line reaches each side");
        assert_eq!(grouped, vec![false, false]);
    }

    /// Two dividers must reach the reader as distinct children. Identical
    /// divider keys used to list one child twice, which the reader kills
    /// the whole tree for.
    #[test]
    fn two_dividers_reach_the_reader_as_distinct_children() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-04T14:00:00+00:00"),
            ],
        );
        let update = crate::a11y::tree_update(&a.chat_view(), None, "Gumicord");
        for (id, node) in &update.nodes {
            let mut seen = std::collections::HashSet::new();
            for c in node.children() {
                assert!(seen.insert(c), "duplicate child {c:?} under {id:?}");
            }
        }
    }

    /// Each day's divider carries its own key. Same-keyed siblings are
    /// what listed one child twice; this pins the fix below the reader.
    #[test]
    fn each_days_divider_carries_its_own_key() {
        fn keys(tree: &UiNode, out: &mut Vec<Option<Key>>) {
            if tree.id == NodeId::LayoutRow
                && tree
                    .children
                    .iter()
                    .any(|c| c.id == NodeId::ChatMessageListDayDivider)
            {
                out.push(tree.key.clone());
            }
            for c in &tree.children {
                keys(c, out);
            }
        }
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-04T14:00:00+00:00"),
            ],
        );
        let tree = a.chat_view();
        let mut found = Vec::new();
        keys(&tree, &mut found);
        assert_eq!(found.len(), 2, "one divider per day");
        assert!(found[0].is_some(), "dividers carry no key");
        assert_ne!(found[0], found[1], "dividers share a key");
    }

    /// The loading row carries its slot and the Loading state, so the
    /// theme can tell it from a message.
    #[test]
    fn the_loading_row_names_its_list() {
        let row = super::rows::loading_row("member_list_loading");
        assert!(row.states.contains(State::Loading));
        assert_eq!(row.key, Some(Key::Slot("member_list_loading")));
        assert!(row.children.iter().any(|c| c.id == NodeId::PrimitiveText));
    }

    /// The date sits centred with a line reaching each side: the spacers
    /// either side of the label hold equal widths on the same height.
    #[test]
    fn the_day_divider_centres_its_label_between_two_lines() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00")],
        );
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);

        let label = placed
            .iter()
            .find(|(id, _)| *id == NodeId::ChatMessageListDayDivider)
            .map(|(_, r)| *r)
            .expect("日付がない");
        let mut lines: Vec<_> = placed
            .iter()
            .filter(|(id, r)| *id == NodeId::LayoutSpacer && r.h > 0.0 && r.h <= 2.0)
            .map(|(_, r)| *r)
            .collect();
        assert_eq!(lines.len(), 2, "両側に線が1本ずつ");
        lines.sort_by(|a, b| a.x.total_cmp(&b.x));
        let (left, right) = (lines[0], lines[1]);
        assert!(
            left.x + left.w <= label.x + 1.0,
            "左の線がラベルに食い込んでいる"
        );
        assert!(
            label.x + label.w <= right.x + 1.0,
            "右の線がラベルに食い込んでいる"
        );
        assert!(
            (left.w - right.w).abs() < 2.0,
            "左右の線が均等でない {left:?} {right:?}"
        );
        assert!(
            (left.y - label.y).abs() < label.h,
            "線と文字が同じ高さにない"
        );
    }

    /// Seven minutes apart starts over, even on the same day.
    #[test]
    fn a_long_pause_starts_over() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-03T12:07:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_eq!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let mut grouped = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(grouped, vec![false, false]);
    }

    /// A reply stands alone and breaks the run; later plain messages can
    /// still group with each other.
    #[test]
    fn a_reply_stands_alone_and_breaks_the_run() {
        let mut a = app(message(None, None));
        let mut replying = stamped(2, 7, "nenneko", "2026-09-03T12:01:00+00:00");
        replying.referenced_message = Some(Box::new(stamped(
            9,
            7,
            "nenneko",
            "2026-09-03T11:00:00+00:00",
        )));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                replying,
                stamped(3, 7, "nenneko", "2026-09-03T12:02:00+00:00"),
                stamped(4, 7, "nenneko", "2026-09-03T12:03:00+00:00"),
            ],
        );

        let tree = a.chat_view();
        let mut grouped = Vec::new();
        let mut dividers = 0;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
            if n.id == NodeId::ChatMessageListDayDivider {
                dividers += 1;
            }
        });
        assert_eq!(grouped, vec![false, false, false, true]);
        assert_eq!(dividers, 1, "same day redrawn after the reply");
    }

    /// The message's own member wins, being newer.
    #[test]
    fn the_member_on_the_message_wins() {
        let mut a = app(message(Some("いまの呼び名"), None));
        a.live.store_mut().remember_member(
            1u64.into(),
            7u64.into(),
            Member {
                nick: Some("むかしの呼び名".to_owned()),
                avatar_hash: None,
                roles: Vec::new(),
                joined_at: None,
                user: None,
            },
        );
        assert_eq!(a.message_rows()[0].author, "いまの呼び名");
    }

    /// Long list items must not overlap the next item, however the text
    /// wraps (regression: multi-line bullets piled onto each other).
    #[test]
    fn long_list_items_do_not_overlap() {
        let mut m = message(None, None);
        m.content = [
            "# テストのお願い",
            "現在gumicordは以下の環境で十分にテストされておらず、テストが必要です",
            "- Android(実機・エミュ問わず全環境でクラッシュ)",
            "- Linux(ディストリビューションは不明だがボットログインまで正常に動作する、クリップボードやセキュアストレージの動作は未確認)",
            "- macOS(動作未確認)",
            "",
            "また以下の環境ではテストが行われていますが動作が不十分です",
            "- iOS(\"フォームボディーが無効です\"エラーでログインできない)",
            "",
            "テストにご協力いただける方は今日の夕方以降に作成される予定のテスターロールをつけていただけると助かります。",
            "というかログをください(Androidの場合はダウンロードフォルダー、それ以外はgumicordのデータフォルダーにあります)",
            "@everyone ",
        ]
        .join("\n");
        let mut a = app(m);
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(900.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let mut shaper = gumicord_render::text::Shaper::new(1.0);
        let r = gumicord_render::layout::layout(
            &tree,
            cx.viewport,
            &mut shaper,
            &gumicord_render::layout::ScrollState::new(),
        );

        let mut rows: Vec<gumicord_render::Rect> = r
            .placed
            .iter()
            .filter(|p| {
                p.node.id == NodeId::LayoutRow
                    && matches!(p.node.key, Some(Key::Slot(s)) if s.starts_with("li"))
            })
            .map(|p| p.rect)
            .collect();
        assert!(rows.len() >= 4, "箇条書きの行がない");
        rows.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        // At least one item wraps, or the test proves nothing.
        let tallest = rows.iter().map(|r| r.h).fold(0.0f32, f32::max);
        let shortest = rows.iter().map(|r| r.h).fold(f32::MAX, f32::min);
        assert!(tallest > shortest + 1.0, "どの項目も折り返していない");
        for w in rows.windows(2) {
            assert!(
                w[1].y >= w[0].y + w[0].h - 0.5,
                "箇条書きが重なっている ({w:?})"
            );
        }
    }

    /// A blank line separates paragraphs wider than a line break does.
    /// Without themed spacing the two read as one break.
    #[test]
    fn blank_lines_separate_paragraphs() {
        let mut m = message(None, None);
        m.content = "first\n\nsecond".to_owned();
        let mut a = app(m);
        // The machine may hold a saved theme; spacing comes from the
        // bundled one under test.
        a.theme = parse_theme_file(DEFAULT_THEME);
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(900.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let mut shaper = gumicord_render::text::Shaper::new(1.0);
        let r = gumicord_render::layout::layout(
            &tree,
            cx.viewport,
            &mut shaper,
            &gumicord_render::layout::ScrollState::new(),
        );

        let mut paras: Vec<gumicord_render::Rect> = r
            .placed
            .iter()
            .filter(|p| p.node.id == NodeId::PrimitiveText && p.node.key == Some(Key::Slot("p")))
            .map(|p| p.rect)
            .collect();
        assert_eq!(paras.len(), 2, "段落がない");
        paras.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        // Margins need no font, so this holds wherever the theme applies.
        assert!(
            paras[1].y - paras[0].bottom() >= 7.5,
            "段落の区切りが改行と変わらない ({paras:?})"
        );
    }

    /// Consecutive blank lines keep their height instead of collapsing
    /// into the paragraph gap.
    #[test]
    fn blank_lines_keep_their_height() {
        let mut m = message(None, None);
        m.content = "a\n\n\n\nb".to_owned();
        let mut a = app(m);
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(900.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let mut shaper = gumicord_render::text::Shaper::new(1.0);
        let r = gumicord_render::layout::layout(
            &tree,
            cx.viewport,
            &mut shaper,
            &gumicord_render::layout::ScrollState::new(),
        );

        let mut paras: Vec<gumicord_render::Rect> = r
            .placed
            .iter()
            .filter(|p| p.node.id == NodeId::PrimitiveText && p.node.key == Some(Key::Slot("p")))
            .map(|p| p.rect)
            .collect();
        assert_eq!(paras.len(), 2, "段落がない");
        paras.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        if paras[1].h == 0.0 {
            eprintln!("フォントが見つからないため、この試験は飛ばす");
            return;
        }
        // "a" plus three empty lines against a single line.
        assert!(
            paras[0].h > paras[1].h * 2.5,
            "空行が潰れている ({paras:?})"
        );
    }
}

#[cfg(test)]
mod member_list_tests {
    use super::*;
    use gumicord_gateway::member_list;
    use serde_json::json;

    /// A guild with one role, and a channel open in it.
    fn app() -> Gumicord {
        let mut a = Gumicord::demo();
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
                roles: vec![gumicord_model::Role {
                    id: 55u64.into(),
                    name: "管理者".to_owned(),
                    position: 3,
                    hoist: true,
                    color: Some(0x00e0_5260),
                }],
            }]);
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 10;
        a
    }

    fn sync(a: &mut Gumicord, items: Vec<serde_json::Value>) {
        let raw = json!({
            "guild_id": "1",
            "member_count": 9,
            "online_count": 2,
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": items }],
        });
        let update = member_list::parse(&raw).expect("読める");
        a.live
            .apply_for_test(live::LiveEvent::Members(Box::new(update)));
    }

    fn person(id: &str, name: &str) -> serde_json::Value {
        json!({ "member": {
            "user": { "id": id, "username": name },
            "roles": [],
            "presence": { "status": "online" },
        }})
    }

    fn texts(node: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        node.walk(&mut |n, _| {
            if let Some(t) = n.content.as_text() {
                out.push(t.to_owned());
            }
        });
        out
    }

    /// Growing the column later would reflow the body under the reader.
    #[test]
    fn the_column_stands_before_anything_arrives() {
        let a = app();

        let empty = a.member_list();
        assert_eq!(empty.id, NodeId::NavMemberList);
        assert!(empty.children.is_empty(), "中身はまだ無い");
        // "Not here yet" is not "nobody here".
        assert!(empty.states.contains(State::Loading));

        let tree = a.build_tree(Panes::Four);
        let mut found = false;
        tree.walk(&mut |n, _| found |= n.id == NodeId::NavMemberList);
        assert!(found, "幅があるうちは列が立っている");
    }

    /// Arrival clears `Loading`.
    #[test]
    fn the_loading_state_goes_away_once_people_arrive() {
        let mut a = app();
        sync(&mut a, vec![person("7", "ねんねこ")]);
        assert!(!a.member_list().states.contains(State::Loading));
    }

    /// Headings show names, never role ids.
    #[test]
    fn headings_come_out_as_names() {
        let mut a = app();
        sync(
            &mut a,
            vec![
                json!({ "group": { "id": "55", "count": 1 } }),
                person("7", "ねんねこ"),
                json!({ "group": { "id": "online", "count": 1 } }),
                person("8", "すぴき"),
            ],
        );

        let list = a.member_list();
        assert_eq!(
            texts(&list),
            vec!["管理者 — 1", "ねんねこ", "オンライン — 1", "すぴき"]
        );
    }

    /// An unresolved role id tells the reader nothing.
    #[test]
    fn a_role_we_cannot_name_is_skipped() {
        let mut a = app();
        sync(
            &mut a,
            vec![
                json!({ "group": { "id": "999999999999999999", "count": 1 } }),
                person("7", "ねんねこ"),
            ],
        );

        assert_eq!(texts(&a.member_list()), vec!["ねんねこ"]);
    }

    /// The role colour rides on the name node; the theme decides where it
    /// lands.
    #[test]
    fn a_role_colour_rides_on_the_name() {
        let mut a = app();
        sync(
            &mut a,
            vec![json!({ "member": {
                "user": { "id": "7", "username": "ねんねこ" },
                "roles": ["55"],
                "presence": { "status": "online" },
            }})],
        );

        let list = a.member_list();
        let name = list
            .children
            .iter()
            .flat_map(|c| c.children.iter())
            .find(|n| n.id == NodeId::NavMemberListItemName)
            .expect("名前がある");
        assert_eq!(name.tint, Some(Color::from_rgb(0x00e0_5260)));
    }

    /// Unknown roles do not get a default colour.
    #[test]
    fn a_member_with_no_known_role_has_no_colour() {
        let mut a = app();
        sync(
            &mut a,
            vec![json!({ "member": {
                "user": { "id": "8", "username": "すぴき" },
                "roles": ["999999999999999999"],
            }})],
        );

        let list = a.member_list();
        let name = list
            .children
            .iter()
            .flat_map(|c| c.children.iter())
            .find(|n| n.id == NodeId::NavMemberListItemName)
            .expect("名前がある");
        assert_eq!(name.tint, None);
    }

    /// The member list folds before chat does.
    #[test]
    fn the_column_is_the_first_thing_to_go() {
        let mut a = app();
        sync(&mut a, vec![person("7", "ねんねこ")]);

        let has = |a: &Gumicord, panes| {
            let tree = a.build_tree(panes);
            let mut found = false;
            tree.walk(&mut |n, _| found |= n.id == NodeId::NavMemberList);
            found
        };
        assert!(has(&a, Panes::Four));
        assert!(!has(&a, Panes::Three));
        assert!(!has(&a, Panes::One));
    }
}

#[cfg(test)]
mod channel_selection_tests {
    use super::*;
    use gumicord_model::{Channel, ChannelKind, Guild};

    fn guild_with_category() -> Guild {
        Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![
                // Categories sort first, so "the first row" picks one.
                Channel {
                    id: 10u64.into(),
                    kind: ChannelKind::GuildCategory,
                    name: Some("カテゴリ".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
                Channel {
                    id: 11u64.into(),
                    kind: ChannelKind::GuildText,
                    name: Some("いっぱん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: Some(10u64.into()),
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
            ],
            roles: Vec::new(),
        }
    }

    /// Categories are headings; the default selection once picked one and
    /// opened a category nobody pressed.
    #[test]
    fn a_category_is_never_selected_by_default() {
        let mut a = Gumicord::demo();
        a.live
            .store_mut()
            .replace_guilds(vec![guild_with_category()]);
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 0;

        a.sync_selection();

        assert_eq!(a.chat.selected_channel, 11, "カテゴリを開こうとしている");
    }

    /// Categories still appear; not openable is not the same as not shown.
    #[test]
    fn the_category_still_appears_in_the_list() {
        let mut a = Gumicord::demo();
        a.live
            .store_mut()
            .replace_guilds(vec![guild_with_category()]);
        a.chat.selected_guild = 1;

        let rows = a.channel_rows();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].category);
        assert_eq!(a.openable_rows().len(), 1);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use gumicord_model::Guild;
    use gumicord_store::FolderRow;

    fn guild(id: u64, name: &str, icon: Option<&str>) -> Guild {
        Guild {
            id: id.into(),
            name: name.to_owned(),
            icon_hash: icon.map(|s| s.to_owned()),
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        }
    }

    /// One folder of three, and one guild outside it.
    fn app_with_folder() -> Gumicord {
        let mut a = Gumicord::demo();
        a.live.store_mut().replace_guilds(vec![
            guild(1, "あ", Some("aaa")),
            guild(2, "い", Some("bbb")),
            guild(3, "う", None),
            guild(4, "え", Some("ddd")),
        ]);
        a.live.store_mut().set_sidebar(vec![
            FolderRow {
                id: Some(100),
                name: None,
                color: Some(0x007c_6cf0),
                guilds: vec![1u64.into(), 2u64.into(), 3u64.into()],
            },
            FolderRow {
                id: None,
                name: None,
                color: None,
                guilds: vec![4u64.into()],
            },
        ]);
        a
    }

    /// Tiles on a folded folder. Counted by `collapsed`, since ordinary
    /// guilds use the same stable ID.
    fn tiles(node: &UiNode) -> usize {
        fn walk(n: &UiNode, found: &mut usize) {
            if n.id == NodeId::NavGuildListItemIcon && n.states.contains(State::Collapsed) {
                *found += 1;
            }
            for c in &n.children {
                walk(c, found);
            }
        }
        let mut found = 0;
        walk(node, &mut found);
        found
    }

    /// The pill appears only when selected, unread or hovered — absent rather
    /// than zero-height, so a visible pill always means something.
    #[test]
    fn the_pill_only_appears_when_it_means_something() {
        fn pills(n: &UiNode, out: &mut Vec<gumicord_uitree::StateSet>) {
            if n.id == NodeId::NavGuildListItemPill {
                out.push(n.states);
            }
            for c in &n.children {
                pills(c, out);
            }
        }

        let mut a = app_with_folder();
        a.chat.selected_guild = 4;

        let mut found = Vec::new();
        pills(&a.guild_list(), &mut found);

        // Only on the selected guild.
        assert_eq!(found.len(), 1);
        assert!(found[0].contains(State::Selected));

        // None selected, none shown.
        a.chat.selected_guild = 0;
        let mut none = Vec::new();
        pills(&a.guild_list(), &mut none);
        assert!(none.is_empty());
    }

    /// The icon is a child of the container, which is wider and leaves a lane
    /// for the pill.
    #[test]
    fn the_item_holds_the_picture_rather_than_being_it() {
        let a = app_with_folder();
        let list = a.guild_list();

        let item = list
            .children
            .iter()
            .find(|n| n.id == NodeId::NavGuildListItem)
            .expect("サーバがある");
        assert!(item.content.as_image().is_none(), "入れ物は絵を持たない");
        assert!(
            item.children
                .iter()
                .any(|c| c.id == NodeId::NavGuildListItemIcon),
            "絵は子である"
        );
        assert!(
            gumicord_render::intrinsic(NodeId::NavGuildListItem).width
                > gumicord_render::intrinsic(NodeId::NavGuildListHome).width,
            "印の通り道のぶん広い"
        );
    }

    /// A folded folder tiles its contents; a box with one initial does not
    /// say which folder it is.
    #[test]
    fn a_closed_folder_shows_what_is_inside() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);

        assert_eq!(tiles(&a.guild_list()), 3);
    }

    /// Tiling an open folder would show the same icons twice.
    #[test]
    fn an_open_folder_does_not_repeat_its_contents() {
        let a = app_with_folder();

        assert_eq!(tiles(&a.guild_list()), 0);
    }

    /// As siblings the background would stop covering them and the folder's
    /// extent would be invisible.
    #[test]
    fn an_open_folder_holds_its_contents() {
        let a = app_with_folder();
        let list = a.guild_list();

        let folder = list
            .children
            .iter()
            .find(|n| n.id == NodeId::NavGuildListFolder)
            .expect("フォルダが無い");
        let inside = folder
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();
        let outside = list
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();

        assert_eq!(inside, 3, "中身がフォルダの中に無い");
        assert_eq!(outside, 1, "フォルダの外のサーバだけが兄弟であるはず");
    }

    /// A folded folder holds no children.
    #[test]
    fn a_closed_folder_holds_nothing() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);
        let list = a.guild_list();

        let items = list
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();
        assert_eq!(items, 1);
    }

    /// Anything beyond the 2x2 is pointless.
    #[test]
    fn only_four_fit() {
        let mut a = Gumicord::demo();
        let many: Vec<_> = (1..=7).map(|i| guild(i, "さ", Some("hash"))).collect();
        a.live.store_mut().replace_guilds(many);
        a.live.store_mut().set_sidebar(vec![FolderRow {
            id: Some(100),
            name: None,
            color: None,
            guilds: (1..=7u64).map(Into::into).collect(),
        }]);
        a.live.store_mut().set_collapsed([100]);

        assert_eq!(tiles(&a.guild_list()), FOLDER_TILES);
    }

    /// A guild without an icon still takes a tile; a gap reads worse.
    #[test]
    fn a_guild_without_an_icon_still_takes_a_tile() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);
        let rows = a.guild_rows();
        let folder = rows.iter().find(|r| r.folder_of_own == Some(100)).unwrap();

        assert_eq!(folder.members.len(), 3);
        assert!(folder.members[2].icon.is_none());
    }
}
