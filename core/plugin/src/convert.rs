//! The host boundary in both directions.
//!
//! Rust hands over structure plus drawable content; bodies are plain data
//! here, while domain facts travel through `ctx`. What comes back is
//! validated node by node: an unknown stable ID discards that plugin's
//! whole output for the frame, while `key`, states, tint and data are
//! inherited from the matching input node  Enever trusted from JS.

use std::collections::HashMap;

use gumicord_uitree::{Content, Key, NodeId, State, UiNode};
use rquickjs::{Array, Ctx, FromJs, IntoJs, Object, Value};

use crate::PluginError;

/// Context handed to `__gumicord_apply` alongside the subtree.
///
/// Resolved by whoever hands the subtree over; `None` arrives as
/// `undefined`. Kept opaque here so resolving it never blocks the host.
#[derive(Debug, Clone, Default)]
pub struct PatchContext {
    pub data: Option<serde_json::Value>,
}

impl PatchContext {
    pub fn empty() -> Self {
        PatchContext { data: None }
    }
}

/// Builds the `{ node, ctx }` pair for one `__gumicord_apply` call.
pub fn apply_args<'js>(
    ctx: Ctx<'js>,
    node: &UiNode,
    patch: &PatchContext,
) -> Result<(Object<'js>, Object<'js>), PluginError> {
    let js_node = node_to_js(ctx.clone(), node).map_err(|e| PluginError::BadPatchOutput {
        id: String::new(),
        reason: format!("cannot hand over the tree: {e}"),
    })?;
    let js_ctx = Object::new(ctx.clone()).map_err(|e| PluginError::BadPatchOutput {
        id: String::new(),
        reason: format!("cannot build the context: {e}"),
    })?;
    if let Some(data) = &patch.data {
        let value = json_to_js(ctx.clone(), data).map_err(|e| PluginError::BadPatchOutput {
            id: String::new(),
            reason: format!("cannot build the context: {e}"),
        })?;
        js_ctx
            .set("data", value)
            .map_err(|e| PluginError::BadPatchOutput {
                id: String::new(),
                reason: format!("cannot build the context: {e}"),
            })?;
    }
    Ok((js_node, js_ctx))
}

/// Reads one patched tree back, inheriting identity from `original`.
pub fn apply_result<'a>(
    ctx: &Ctx<'a>,
    original: &UiNode,
    patched: Value<'a>,
) -> Result<UiNode, PluginError> {
    let mut index = HashMap::new();
    index_original(original, &mut index);
    js_to_node(ctx, &patched, &index)
}

/// Nodes of the input tree by `(id, key)`, so a returned node can inherit
/// the identity JS must never set: key, states, tint, data and anchor.
fn index_original<'a>(node: &'a UiNode, index: &mut HashMap<(String, String), &'a UiNode>) {
    index
        .entry((node.id.as_str().to_owned(), key_string(&node.key)))
        .or_insert(node);
    for child in &node.children {
        index_original(child, index);
    }
}

/// Rust node ↁEplain JS object matching the SDK's `UINode`, plus the
/// drawable content so wrapped nodes survive the trip back.
pub fn node_to_js<'js>(ctx: Ctx<'js>, node: &UiNode) -> rquickjs::Result<Object<'js>> {
    let js = Object::new(ctx.clone())?;
    js.set("id", node.id.as_str())?;
    if let Some(key) = key_string_opt(&node.key) {
        js.set("key", key)?;
    }
    let states: Vec<&str> = node.states.iter().map(State::as_str).collect();
    if !states.is_empty() {
        let list = Array::new(ctx.clone())?;
        for (i, s) in states.iter().enumerate() {
            list.set(i, *s)?;
        }
        js.set("states", list)?;
    }
    if let Some(tint) = node.tint {
        js.set(
            "tint",
            format!("#{:02x}{:02x}{:02x}", tint.r, tint.g, tint.b),
        )?;
    }
    if node.content != Content::None {
        js.set("content", content_to_js(ctx.clone(), &node.content)?)?;
    }
    if !node.children.is_empty() {
        let kids = Array::new(ctx.clone())?;
        for (i, child) in node.children.iter().enumerate() {
            kids.set(i, node_to_js(ctx.clone(), child)?)?;
        }
        js.set("children", kids)?;
    }
    Ok(js)
}

/// Drawable content as plain values. Everything here must survive the trip
/// back through `content_from_js` unchanged.
fn content_to_js<'js>(ctx: Ctx<'js>, content: &Content) -> rquickjs::Result<Value<'js>> {
    let obj = Object::new(ctx.clone())?;
    match content {
        Content::None => Ok(Value::new_undefined(ctx.clone())),
        Content::Text(s) => {
            obj.set("kind", "text")?;
            obj.set("text", s.clone())?;
            Ok(as_value(&obj))
        }
        Content::Icon(name) => {
            obj.set("kind", "icon")?;
            obj.set("name", name.clone())?;
            Ok(as_value(&obj))
        }
        Content::Image(url) => {
            obj.set("kind", "image")?;
            obj.set("url", url.clone())?;
            Ok(as_value(&obj))
        }
        Content::Qr(value) => {
            obj.set("kind", "qr")?;
            obj.set("value", value.clone())?;
            Ok(as_value(&obj))
        }
        Content::Rich(spans) => {
            obj.set("kind", "rich")?;
            let list = Array::new(ctx.clone())?;
            for (i, span) in spans.iter().enumerate() {
                list.set(i, span_to_js(ctx.clone(), span)?)?;
            }
            obj.set("spans", list)?;
            Ok(as_value(&obj))
        }
        Content::Editable(e) => {
            obj.set("kind", "editable")?;
            edit_to_js(ctx.clone(), e, &obj)?;
            Ok(as_value(&obj))
        }
    }
}

fn span_to_js<'js>(ctx: Ctx<'js>, span: &gumicord_uitree::Span) -> rquickjs::Result<Value<'js>> {
    let num = |n: f64| Value::new_number(ctx.clone(), n);
    let obj = Object::new(ctx.clone())?;
    obj.set("text", span.text.clone())?;
    if let Some(font) = &span.font {
        let f = Object::new(ctx.clone())?;
        if let Some(family) = &font.family {
            f.set("family", family.clone())?;
        }
        if let Some(size) = font.size {
            f.set("size", num(size as f64))?;
        }
        if let Some(line_height) = font.line_height {
            f.set("lineHeight", num(line_height as f64))?;
        }
        if let Some(weight) = font.weight {
            f.set("weight", num(weight as f64))?;
        }
        if let Some(italic) = font.italic {
            f.set("italic", italic)?;
        }
        if let Some(spacing) = font.letter_spacing {
            f.set("letterSpacing", num(spacing as f64))?;
        }
        obj.set("font", f)?;
    }
    if let Some(color) = &span.color {
        let c = Object::new(ctx.clone())?;
        c.set("r", num(color.r as f64))?;
        c.set("g", num(color.g as f64))?;
        c.set("b", num(color.b as f64))?;
        c.set("a", num(color.a as f64))?;
        obj.set("color", c)?;
    }
    obj.set("under", span.line.under)?;
    obj.set("through", span.line.through)?;
    obj.set("hidden", span.hidden)?;
    obj.set("revealed", span.revealed)?;
    if let Some(link) = &span.link {
        obj.set("link", link.clone())?;
    }
    if let Some(image) = &span.image {
        obj.set("image", image.clone())?;
    }
    Ok(as_value(&obj))
}

fn edit_to_js<'js>(
    ctx: Ctx<'js>,
    e: &gumicord_uitree::Editable,
    obj: &Object<'js>,
) -> rquickjs::Result<()> {
    let num = |n: f64| Value::new_number(ctx.clone(), n);
    obj.set("text", e.text.clone())?;
    obj.set("caret", num(e.caret as f64))?;
    let selection = Object::new(ctx.clone())?;
    selection.set("start", num(e.selection.start as f64))?;
    selection.set("end", num(e.selection.end as f64))?;
    obj.set("selection", selection)?;
    if let Some(range) = &e.composing {
        let composing = Object::new(ctx.clone())?;
        composing.set("start", num(range.start as f64))?;
        composing.set("end", num(range.end as f64))?;
        obj.set("composing", composing)?;
    }
    obj.set("placeholder", e.placeholder.clone())?;
    Ok(())
}

fn as_value<'a, T: AsRef<Value<'a>>>(v: &T) -> Value<'a> {
    v.as_ref().clone()
}

/// Plain JS object ↁERust node, inheriting identity from the input tree.
pub fn js_to_node<'a>(
    ctx: &Ctx<'a>,
    value: &Value<'a>,
    index: &HashMap<(String, String), &UiNode>,
) -> Result<UiNode, PluginError> {
    let bad = |reason: String| PluginError::BadPatchOutput {
        id: String::new(),
        reason,
    };
    let obj = object_of(value).ok_or_else(|| bad("a patch returned no object".into()))?;
    let id: String = obj.get("id").map_err(|_| bad("a node has no id".into()))?;
    let node_id: NodeId = id
        .parse()
        .map_err(|_| bad(format!("unknown stable ID: {id}")))?;
    let key: Option<String> = obj.get("key").unwrap_or(None);
    let key_str = key.clone().unwrap_or_default();

    let mut node = UiNode::new(node_id);
    if let Some(found) = index.get(&(id.clone(), key_str)) {
        let o = found;
        if o.id == node_id {
            node.key.clone_from(&o.key);
            node.states = o.states;
            node.tint = o.tint;
            node.data = o.data;
            node.anchor = o.anchor;
        }
    }
    node.content = content_from(ctx, node_id, &obj).map_err(bad)?;

    if obj.contains_key("children").unwrap_or(false) {
        let kids: Array = obj
            .get("children")
            .map_err(|_| bad("children is no array".into()))?;
        let mut children = Vec::with_capacity(kids.len());
        for i in 0..kids.len() {
            let child: Value = kids
                .get(i)
                .map_err(|e| bad(format!("child {i} unreadable: {e}")))?;
            children.push(js_to_node(ctx, &child, index)?);
        }
        node.children = children;
    }
    Ok(node)
}

/// Content from a returned node: the `content` it carries wins, otherwise
/// the `props` the SDK constructors produce. Anything else draws nothing.
fn content_from<'a>(ctx: &Ctx<'a>, id: NodeId, obj: &Object<'a>) -> Result<Content, String> {
    if obj.contains_key("content").unwrap_or(false) {
        let value: Value = obj
            .get("content")
            .map_err(|_| "content is unreadable".to_owned())?;
        return content_from_js(ctx, &value);
    }
    let props: Option<Object> = obj.get("props").ok().and_then(|v: Value| object_of(&v));
    let text_prop = |name: &str| -> Option<String> {
        let props = props.as_ref()?;
        let value: Value = props.get(name).ok()?;
        if value.is_string() {
            String::from_js(ctx, value).ok()
        } else {
            None
        }
    };
    // Exact shapes first, then the generic text-ish props any node  E    // including `plugin.*`  Emay carry.
    if id == NodeId::PrimitiveIcon
        && let Some(name) = text_prop("name")
    {
        return Ok(Content::Icon(name));
    }
    if id == NodeId::PrimitiveImage
        && let Some(url) = text_prop("url")
    {
        return Ok(Content::Image(url));
    }
    if id == NodeId::PrimitiveQr
        && let Some(value) = text_prop("value")
    {
        return Ok(Content::Qr(value));
    }
    for name in ["value", "text", "label"] {
        if let Some(s) = text_prop(name) {
            return Ok(Content::Text(s));
        }
    }
    Ok(Content::None)
}

/// Reads back exactly what `content_to_js` writes; anything else is a
/// plugin bug, reported loudly rather than drawn wrong.
fn content_from_js<'a>(ctx: &Ctx<'a>, value: &Value<'a>) -> Result<Content, String> {
    let bad = |what: &str| format!("content.{what} unreadable");
    let obj = object_of(value).ok_or_else(|| bad("content"))?;
    let kind: String = obj.get("kind").map_err(|_| bad("kind"))?;
    let get_text = |name: &str| -> Result<String, String> {
        let value: Value = obj.get(name).map_err(|_| bad(name))?;
        if value.is_string() {
            String::from_js(ctx, value).map_err(|_| bad(name))
        } else {
            Err(bad(name))
        }
    };
    let number = |obj: &Object, name: &str| -> Result<f64, String> {
        let value: Value = obj.get(name).map_err(|_| bad(name))?;
        value.as_number().ok_or_else(|| bad(name))
    };
    let whole = |obj: &Object, name: &str, max: f64| -> Result<usize, String> {
        let n = number(obj, name)?;
        if n < 0.0 || n.fract() != 0.0 || n > max {
            return Err(bad(name));
        }
        Ok(n as usize)
    };
    let child = |name: &str| -> Result<Object, String> {
        let value: Value = obj.get(name).map_err(|_| bad(name))?;
        object_of(&value).ok_or_else(|| bad(name))
    };
    match kind.as_str() {
        "text" => Ok(Content::Text(get_text("text")?)),
        "icon" => Ok(Content::Icon(get_text("name")?)),
        "image" => Ok(Content::Image(get_text("url")?)),
        "qr" => Ok(Content::Qr(get_text("value")?)),
        "rich" => {
            let items: Value = obj.get("spans").map_err(|_| bad("spans"))?;
            if !items.is_array() {
                return Err(bad("spans"));
            }
            let list = rquickjs::Array::from_js(ctx, items).map_err(|_| bad("spans"))?;
            let mut spans = Vec::with_capacity(list.len());
            for i in 0..list.len() {
                let raw: Value = list.get(i).map_err(|_| bad("spans"))?;
                spans.push(span_from_js(ctx, &raw)?);
            }
            Ok(Content::Rich(spans))
        }
        "editable" => {
            let text = get_text("text")?;
            let caret = whole(&obj, "caret", u32::MAX as f64)?;
            let selection = child("selection")?;
            let start = whole(&selection, "start", u32::MAX as f64)?;
            let end = whole(&selection, "end", u32::MAX as f64)?;
            let composing = if obj.contains_key("composing").unwrap_or(false) {
                let part = child("composing")?;
                Some(whole(&part, "start", u32::MAX as f64)?..whole(&part, "end", u32::MAX as f64)?)
            } else {
                None
            };
            let placeholder = get_text("placeholder")?;
            Ok(Content::Editable(gumicord_uitree::Editable {
                text,
                caret,
                selection: start..end,
                composing,
                placeholder,
            }))
        }
        other => Err(format!("unknown content kind: {other}")),
    }
}

fn span_from_js<'a>(ctx: &Ctx<'a>, value: &Value<'a>) -> Result<gumicord_uitree::Span, String> {
    use gumicord_uitree::Line;
    use gumicord_uitree::value::{Color, Font};
    let bad = |what: &str| format!("span.{what} unreadable");
    let obj = object_of(value).ok_or_else(|| bad("span"))?;
    let text_of = |name: &str| -> Result<String, String> {
        let value: Value = obj.get(name).map_err(|_| bad(name))?;
        if value.is_string() {
            String::from_js(ctx, value).map_err(|_| bad(name))
        } else {
            Err(bad(name))
        }
    };
    let required_flag = |name: &str| -> Result<bool, String> {
        let value: Value = obj.get(name).map_err(|_| bad(name))?;
        if value.is_bool() {
            Ok(value.as_bool().unwrap_or(false))
        } else {
            Err(bad(name))
        }
    };
    let text = text_of("text")?;
    let font = if obj.contains_key("font").unwrap_or(false) {
        let raw: Value = obj.get("font").map_err(|_| bad("font"))?;
        let f = object_of(&raw).ok_or_else(|| bad("font"))?;
        let opt_text = |name: &str| -> Result<Option<String>, String> {
            if !f.contains_key(name).unwrap_or(false) {
                return Ok(None);
            }
            let value: Value = f.get(name).map_err(|_| bad(name))?;
            if value.is_string() {
                String::from_js(ctx, value).map(Some).map_err(|_| bad(name))
            } else {
                Err(bad(name))
            }
        };
        let opt_num = |name: &str| -> Result<Option<f64>, String> {
            if !f.contains_key(name).unwrap_or(false) {
                return Ok(None);
            }
            let value: Value = f.get(name).map_err(|_| bad(name))?;
            value.as_number().map(Some).ok_or_else(|| bad(name))
        };
        let opt_bool = |name: &str| -> Result<Option<bool>, String> {
            if !f.contains_key(name).unwrap_or(false) {
                return Ok(None);
            }
            let value: Value = f.get(name).map_err(|_| bad(name))?;
            if value.is_bool() {
                Ok(Some(value.as_bool().unwrap_or(false)))
            } else {
                Err(bad(name))
            }
        };
        Some(Font {
            family: opt_text("family")?,
            size: opt_num("size")?.map(|n| n as f32),
            line_height: opt_num("lineHeight")?.map(|n| n as f32),
            weight: opt_num("weight")?.map(|n| n as u16),
            italic: opt_bool("italic")?,
            letter_spacing: opt_num("letterSpacing")?.map(|n| n as f32),
        })
    } else {
        None
    };
    let color = if obj.contains_key("color").unwrap_or(false) {
        let raw: Value = obj.get("color").map_err(|_| bad("color"))?;
        let c = object_of(&raw).ok_or_else(|| bad("color"))?;
        let byte = |name: &str| -> Result<u8, String> {
            let value: Value = c.get(name).map_err(|_| bad(name))?;
            let n = value.as_number().ok_or_else(|| bad(name))?;
            if !(0.0..=255.0).contains(&n) || n.fract() != 0.0 {
                return Err(bad(name));
            }
            Ok(n as u8)
        };
        Some(Color {
            r: byte("r")?,
            g: byte("g")?,
            b: byte("b")?,
            a: byte("a")?,
        })
    } else {
        None
    };
    Ok(gumicord_uitree::Span {
        text,
        font,
        color,
        line: Line {
            under: required_flag("under")?,
            through: required_flag("through")?,
        },
        hidden: required_flag("hidden")?,
        revealed: required_flag("revealed")?,
        link: {
            if obj.contains_key("link").unwrap_or(false) {
                Some(text_of("link")?)
            } else {
                None
            }
        },
        image: {
            if obj.contains_key("image").unwrap_or(false) {
                Some(text_of("image")?)
            } else {
                None
            }
        },
    })
}

fn object_of<'a>(value: &Value<'a>) -> Option<Object<'a>> {
    if value.is_object() && !value.is_array() && !value.is_function() {
        Value::clone(value).into_object()
    } else {
        None
    }
}

fn key_string(key: &Option<Key>) -> String {
    key_string_opt(key).unwrap_or_default()
}

fn key_string_opt(key: &Option<Key>) -> Option<String> {
    match key {
        None => None,
        Some(Key::Id(n)) => Some(n.to_string()),
        Some(Key::Slot(s)) => Some(s.to_string()),
        Some(Key::Index(i)) => Some(i.to_string()),
    }
}

/// `serde_json` values as native JS values; no string round trip.
fn json_to_js<'js>(ctx: Ctx<'js>, value: &serde_json::Value) -> rquickjs::Result<Value<'js>> {
    use serde_json::Value as J;
    match value {
        J::Null => Ok(Value::new_null(ctx.clone())),
        J::Bool(b) => Ok(Value::new_bool(ctx.clone(), *b)),
        J::Number(n) => Ok(Value::new_number(
            ctx.clone(),
            n.as_f64().unwrap_or(f64::NAN),
        )),
        J::String(s) => s.clone().into_js(&ctx),
        J::Array(items) => {
            let out = Array::new(ctx.clone())?;
            for (i, item) in items.iter().enumerate() {
                out.set(i, json_to_js(ctx.clone(), item)?)?;
            }
            Ok(as_value(&out))
        }
        J::Object(map) => {
            let out = Object::new(ctx.clone())?;
            for (k, v) in map {
                out.set(k.as_str(), json_to_js(ctx.clone(), v)?)?;
            }
            Ok(as_value(&out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_uitree::{Content, NodeId, UiNode};
    use rquickjs::{Context, Runtime};

    fn tree() -> UiNode {
        UiNode::text(NodeId::ChatMessageContent, "hi").with_id_key(7)
    }

    /// A round trip keeps structure; identity stays.
    #[test]
    fn nodes_round_trip_through_js() {
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let js = node_to_js(ctx.clone(), &tree()).unwrap();
            let back = apply_result(&ctx, &tree(), as_value(&js)).unwrap();
            assert_eq!(back.id, NodeId::ChatMessageContent);
            assert_eq!(back.content.as_text(), Some("hi"));
            assert_eq!(back.key, tree().key, "key is inherited");
            assert_eq!(back.children.len(), 0);
        });
    }

    /// A wrapped node keeps its body: spread children carry content back.
    #[test]
    fn wrapped_nodes_keep_their_bodies() {
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let js = node_to_js(ctx.clone(), &tree()).unwrap();
            ctx.globals().set("__input", js).unwrap();
            let wrapped: Value = ctx
                .eval(r#"({ id: "layout.column", children: [__input] })"#)
                .unwrap();
            let back = apply_result(&ctx, &tree(), wrapped).unwrap();
            assert_eq!(back.id, NodeId::LayoutColumn);
            assert_eq!(back.children.len(), 1);
            assert_eq!(back.children[0].content.as_text(), Some("hi"));
            assert_eq!(back.children[0].key, tree().key);
        });
    }

    /// Plugin-built nodes gain drawable content from their props.
    #[test]
    fn props_become_content() {
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let made: Value = ctx
                .eval(
                    r#"({
                    id: "primitive.badge", props: { text: "BOT" },
                    children: [{ id: "primitive.text", props: { value: "x" } }],
                })"#,
                )
                .unwrap();
            let back = apply_result(&ctx, &tree(), made).unwrap();
            assert_eq!(back.id, NodeId::PrimitiveBadge);
            assert_eq!(back.content.as_text(), Some("BOT"));
            assert_eq!(back.children.len(), 1);
            assert_eq!(back.children[0].content.as_text(), Some("x"));
        });
    }

    /// Unknown IDs discard the output, never half of it.
    #[test]
    fn unknown_ids_fail_the_whole_output() {
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let made: Value = ctx.eval(r#"({ id: "bogus.node" })"#).unwrap();
            let err = apply_result(&ctx, &tree(), made).unwrap_err();
            assert!(matches!(err, PluginError::BadPatchOutput { .. }), "{err}");
        });
    }

    /// Functions cannot cross back; they fall away silently.
    #[test]
    fn functions_fall_away_silently() {
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let made: Value = ctx
                .eval(r#"({ id: "primitive.button", props: { label: "Go", onPress: () => {} } })"#)
                .unwrap();
            let back = apply_result(&ctx, &tree(), made).unwrap();
            assert_eq!(back.content.as_text(), Some("Go"));
        });
    }

    /// Every content variant survives the trip back unchanged.
    #[test]
    fn every_content_variant_round_trips() {
        use gumicord_uitree::{Editable, Span};
        let rt = Runtime::new().unwrap();
        let context = Context::full(&rt).unwrap();
        context.with(|ctx| {
            let variants = [
                Content::Icon("smile".to_owned()),
                Content::Image("https://cdn.example.com/a.png".to_owned()),
                Content::Qr("qr-data".to_owned()),
                Content::Rich(vec![gumicord_uitree::Span {
                    text: "x".to_owned(),
                    ..Span::default()
                }]),
                Content::Editable(Editable {
                    text: "edit me".to_owned(),
                    caret: 2,
                    selection: 1..2,
                    composing: None,
                    placeholder: "type".to_owned(),
                }),
            ];
            for content in variants {
                let node = UiNode::new(NodeId::ChatMessageContent).with_content(content.clone());
                let js = node_to_js(ctx.clone(), &node).unwrap();
                let back = apply_result(&ctx, &node, as_value(&js)).unwrap();
                assert_eq!(back.content, content);
            }
        });
    }
}
