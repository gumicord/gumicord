//! Chat composer: input bar, status line and the assembled chat view.
use super::Composing;
use gumicord_model::ChannelId;
use gumicord_uitree::{Editable, Key, NodeId, State, UiNode};

impl crate::Gumicord {
    pub(crate) fn chat_view(&self) -> UiNode {
        let channels = self.openable_rows();
        let channel = channels
            .iter()
            .find(|c| c.id == self.chat.selected_channel)
            .or(channels.first());

        let (id, name, icon, topic) = match channel {
            Some(c) => (c.id, c.name.clone(), c.icon, c.topic.clone()),
            None => (0, String::new(), "channel.text", None),
        };

        let mut header = UiNode::new(NodeId::ChatHeader).with_data(id);
        // First, so it sits left of the channel name: only built while
        // the guild list hides.
        header = header.child_if(!self.panes().guilds(), || {
            UiNode::new(NodeId::PrimitiveButton)
                .with_key(Key::Slot(crate::BACK_OPEN))
                .with_state_if(
                    self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot(crate::BACK_OPEN))),
                    State::Hover,
                )
                .child(UiNode::icon(NodeId::PrimitiveIcon, crate::BACK_ICON))
        });
        let header = header
            .child(UiNode::icon(NodeId::PrimitiveIcon, icon))
            .child(UiNode::text(NodeId::ChatHeaderTitle, &name).with_data(id))
            .child(UiNode::text(NodeId::ChatHeaderTopic, topic.unwrap_or_default()).with_data(id))
            // Last, so it draws and hits above the header: only built
            // while the member pane hides.
            .child_if(!self.panes().members(), || {
                UiNode::new(NodeId::PrimitiveButton)
                    .with_key(Key::Slot(crate::MEMBERS_OPEN))
                    .with_state_if(
                        self.is_hovered(
                            NodeId::PrimitiveButton,
                            Some(&Key::Slot(crate::MEMBERS_OPEN)),
                        ),
                        State::Hover,
                    )
                    .child(UiNode::icon(NodeId::PrimitiveIcon, crate::MEMBERS_ICON))
            });

        // A day always starts labelled, and one header covers a run: same
        // author, same day, close together. Anything else starts over.
        // A reply always stands alone and breaks the run, whatever follows.
        // Dividers track the day on their own: a broken run must not
        // redraw the date.
        let rows = self.message_rows();
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        // Older pages arrive above: while one is on its way, a row at
        // the top says so instead of ending the list in silence.
        let channel = ChannelId::from(self.chat.selected_channel);
        if self.live.paging_older(channel) || self.live.is_loading(channel) {
            messages = messages.child(super::rows::loading_row("message_list_loading"));
        }
        let mut prev: Option<(&str, &str, i64)> = None;
        let mut divided_day = "";
        for m in &rows {
            if !m.day.is_empty() && m.day != divided_day {
                messages = messages.child(Self::day_divider(&m.day));
                divided_day = &m.day;
            }
            let grouped = match prev {
                Some((author, day, unix)) if m.reply.is_none() => {
                    author == m.author && crate::time::continues(day, unix, &m.day, m.unix)
                }
                _ => false,
            };
            messages = messages.child(self.message(m, grouped));
            prev = if m.reply.is_some() {
                None
            } else {
                Some((&m.author, &m.day, m.unix))
            };
        }
        messages = messages.children(self.scrollbar(NodeId::ChatMessageList));

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                self.status_line(),
            ))
            .child(
                UiNode::new(NodeId::ChatInput)
                    // What the composer is doing has to be visible: sending a
                    // new message while meaning to edit cannot be undone.
                    .child_if(self.chat.composing != Composing::New, || {
                        self.composing_bar()
                    })
                    .child(
                        UiNode::editable(
                            NodeId::ChatInputField,
                            Editable {
                                text: self.chat.input.text().to_owned(),
                                caret: self.chat.input.caret(),
                                selection: self.chat.input.selection(),
                                composing: self.chat.input.composing(),
                                placeholder: if name.is_empty() {
                                    "メッセージを送信".to_owned()
                                } else {
                                    format!("#{name} へメッセージを送信")
                                },
                            },
                        )
                        .with_state_if(self.chat.input_focused, State::Focus),
                    ),
            )
    }

    /// Cancels a reply or an edit.
    ///
    /// The draft survives cancelling a reply, which only removed a recipient
    /// from text that is still sendable. It does not survive cancelling an
    /// edit, where the field holds the original message rather than anything
    /// the user wrote.
    pub(crate) fn stop_composing(&mut self) -> bool {
        match self.chat.composing {
            Composing::New => false,
            Composing::Reply(_) => {
                self.chat.composing = Composing::New;
                true
            }
            Composing::Edit(_) => {
                self.chat.composing = Composing::New;
                self.chat.input.take();
                true
            }
        }
    }

    /// The line above the composer, naming who is being replied to. "Replying"
    /// alone stops meaning anything once the list has scrolled.
    pub(crate) fn composing_bar(&self) -> UiNode {
        let (verb, slot) = match self.chat.composing {
            Composing::Reply(_) => ("返信", "reply"),
            Composing::Edit(_) => ("編集", "edit"),
            Composing::New => ("", "none"),
        };
        let who = self
            .chat
            .composing
            .target()
            .and_then(|id| self.message_rows().into_iter().find(|m| m.id == id))
            .map(|m| m.author);

        let text = match (&self.chat.composing, who) {
            (Composing::Reply(_), Some(a)) => format!("{a} に{verb}中"),
            // A scrolled-away target cannot be resolved; still show the state.
            (Composing::Reply(_), None) => format!("{verb}中"),
            _ => format!("{verb}中"),
        };
        UiNode::new(NodeId::ChatInputToolbar)
            .with_key(Key::Slot(slot))
            .child(UiNode::text(NodeId::PrimitiveText, text).with_key(Key::Slot(slot)))
            // A spacer rather than a written margin, which would stop
            // matching once the theme changes the bar's padding.
            .child(UiNode::new(NodeId::LayoutSpacer))
            // Escape works too, but without a visible way out this looks like
            // a state with no exit.
            .child(
                UiNode::new(NodeId::PrimitiveButton)
                    .with_key(Key::Slot(crate::CANCEL_COMPOSING))
                    .with_state_if(
                        self.is_hovered(
                            NodeId::PrimitiveButton,
                            Some(&Key::Slot(crate::CANCEL_COMPOSING)),
                        ),
                        State::Hover,
                    )
                    .child(UiNode::icon(NodeId::PrimitiveIcon, "close")),
            )
    }

    /// The line below the list. Silent while connected; announcing the normal
    /// case buries the abnormal one.
    pub(crate) fn status_line(&self) -> String {
        if let Some(hint) = self.live.link().hint() {
            return format!("  {hint}");
        }
        if self.uses_live() {
            let channel = ChannelId::from(self.chat.selected_channel);
            if self.live.is_loading(channel) {
                return "  読み込んでいます…".to_owned();
            }
            return crate::typing_line(&self.live.typing_in(channel));
        }
        "  みどり が入力中…".to_owned()
    }

    /// One id per day label: sibling dividers must not share a key, or
    /// the reader rejects the tree for listing one child twice.
    fn day_id(day: &str) -> u64 {
        // FNV-1a: deterministic across frames, unlike the default hasher.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in day.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// A day divider: the date centred with a line reaching both sides.
    /// The lines are spacers the theme paints; soaking the row's remainder
    /// keeps the label centred whatever the width.
    pub(crate) fn day_divider(day: &str) -> UiNode {
        UiNode::new(NodeId::LayoutRow)
            .with_key(Key::Id(Self::day_id(day)))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
            .child(UiNode::text(NodeId::ChatMessageListDayDivider, day))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
    }
}
