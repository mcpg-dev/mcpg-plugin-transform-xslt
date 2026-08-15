//! Self-contained XML→JSON projection for the `xml_to_json` output mode.
//!
//! Mirrors the `dev.mcpg.transform.xml` plugin's convention so the two plugins
//! agree on shape: a text-only element becomes a string; an element with
//! attributes or children becomes an object with `@attr` keys, a `#text` key for
//! mixed character content, child tags as keys, and repeated child tags
//! collapsed into an array. Kept here rather than depending on the xml plugin so
//! the xslt crate stays standalone.

use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
use serde_json::{Map, Value};

/// A partially-built element: its attributes, child elements, and accumulated
/// text. Finalised into a JSON value when its end tag is seen.
#[derive(Default)]
struct Node {
    attrs: Vec<(String, String)>,
    children: Map<String, Value>,
    text: String,
}

fn attrs_of(e: &BytesStart<'_>) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|e| format!("xml attribute: {e}"))?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let val = a
            .unescape_value()
            .map_err(|e| format!("xml attribute value: {e}"))?
            .into_owned();
        out.push((key, val));
    }
    Ok(out)
}

fn finalize(node: Node) -> Value {
    let text = node.text.trim();
    if node.attrs.is_empty() && node.children.is_empty() {
        return Value::String(text.to_owned());
    }
    let mut map = Map::new();
    for (k, v) in node.attrs {
        map.insert(format!("@{k}"), Value::String(v));
    }
    for (k, v) in node.children {
        map.insert(k, v);
    }
    if !text.is_empty() {
        map.insert("#text".to_owned(), Value::String(text.to_owned()));
    }
    Value::Object(map)
}

/// Attach a finalised child under `key`, collapsing repeated tags into an array.
fn insert_child(map: &mut Map<String, Value>, key: String, val: Value) {
    match map.get_mut(&key) {
        Some(Value::Array(arr)) => arr.push(val),
        Some(existing) => {
            let prev = existing.take();
            *existing = Value::Array(vec![prev, val]);
        }
        None => {
            map.insert(key, val);
        }
    }
}

/// Resolve a `&entity;` reference (the five predefined entities + numeric).
fn resolve_entity(r: &BytesRef<'_>) -> Result<String, String> {
    if r.is_char_ref() {
        return match r.resolve_char_ref() {
            Ok(Some(c)) => Ok(c.to_string()),
            Ok(None) => Ok(String::new()),
            Err(e) => Err(format!("xml char ref: {e}")),
        };
    }
    let name = r.decode().map_err(|e| format!("xml entity: {e}"))?;
    match name.as_ref() {
        "amp" => Ok("&".to_owned()),
        "lt" => Ok("<".to_owned()),
        "gt" => Ok(">".to_owned()),
        "quot" => Ok("\"".to_owned()),
        "apos" => Ok("'".to_owned()),
        other => Err(format!("unknown XML entity &{other};")),
    }
}

/// Parse an XML string into a JSON value. The document's root tag becomes a key
/// in the returned object. Malformed XML surfaces as an `Err`.
pub fn xml_to_json(input: &str) -> Result<Value, String> {
    let mut reader = Reader::from_str(input);
    // A sentinel root collects the top-level element(s) so the document's root
    // tag becomes a key in the result.
    let mut stack: Vec<Node> = vec![Node::default()];

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                stack.push(Node {
                    attrs: attrs_of(&e)?,
                    ..Node::default()
                });
            }
            Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let node = Node {
                    attrs: attrs_of(&e)?,
                    ..Node::default()
                };
                let parent = stack.last_mut().ok_or("xml: unbalanced tags")?;
                insert_child(&mut parent.children, tag, finalize(node));
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let node = stack.pop().ok_or("xml: unbalanced end tag")?;
                if stack.is_empty() {
                    return Err("xml: unbalanced end tag".to_owned());
                }
                let val = finalize(node);
                let parent = stack.last_mut().expect("non-empty after is_empty check");
                insert_child(&mut parent.children, tag, val);
            }
            Ok(Event::Text(e)) => {
                let t = e.decode().map_err(|e| format!("xml text: {e}"))?;
                stack
                    .last_mut()
                    .expect("sentinel always present")
                    .text
                    .push_str(&t);
            }
            Ok(Event::CData(e)) => {
                let t = e.decode().map_err(|e| format!("xml cdata: {e}"))?;
                stack
                    .last_mut()
                    .expect("sentinel always present")
                    .text
                    .push_str(&t);
            }
            Ok(Event::GeneralRef(r)) => {
                let t = resolve_entity(&r)?;
                stack
                    .last_mut()
                    .expect("sentinel always present")
                    .text
                    .push_str(&t);
            }
            Ok(Event::Eof) => break,
            // Declarations, comments, PIs, DOCTYPE carry no data we map.
            Ok(_) => {}
            Err(e) => return Err(format!("xml parse error: {e}")),
        }
    }

    if stack.len() != 1 {
        return Err("xml: unclosed element(s)".to_owned());
    }
    let root = stack.pop().expect("sentinel");
    Ok(Value::Object(root.children))
}
