use mcpg_plugin_protocol::{PluginContext, PluginIdentity, TransformResult};
use mcpg_plugin_sdk::ffi::SyncTransform;
use serde_json::json;

use super::XsltTransform;

fn ctx() -> PluginContext {
    PluginContext {
        request_id: "t".into(),
        session_id: None,
        tool_name: "x".into(),
        surface: "tool".into(),
        identity: PluginIdentity {
            kind: "anonymous".into(),
            trust_level: "unauthenticated".into(),
            subject_id: None,
            auth_provider: None,
            issuer: None,
            roles: Vec::new(),
            groups: Vec::new(),
            scopes: Vec::new(),
            attributes: Default::default(),
        },
        transport: "http".into(),
    }
}

/// A stylesheet that maps `<a><b>x</b></a>` to the literal element `<out>x</out>`.
const A_TO_OUT: &str = r#"<xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
  <xsl:template match='/a'><out><xsl:value-of select='b'/></out></xsl:template>
</xsl:stylesheet>"#;

#[test]
fn golden_maps_element() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT });
    let input = json!("<a><b>x</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<out>x</out>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn extracts_text_value() {
    let p = XsltTransform::new("{}");
    let cfg = json!({
        "stylesheet": r#"<xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
  <xsl:template match='/a'><xsl:value-of select='b'/></xsl:template>
</xsl:stylesheet>"#
    });
    let input = json!("<a><b>hello</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("hello")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn identity_copy_round_trip() {
    let p = XsltTransform::new("{}");
    let cfg = json!({
        "stylesheet": r#"<xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
  <xsl:template match='/'><xsl:copy-of select='a'/></xsl:template>
</xsl:stylesheet>"#
    });
    let input = json!("<a><b>x</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<a><b>x</b></a>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn input_path_selects_xml_field() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "input": "xml" });
    let input = json!({ "xml": "<a><b>x</b></a>", "other": 1 });
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<out>x</out>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn input_dotted_path_selects_nested_field() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "input": "steps.call.output" });
    let input = json!({ "steps": { "call": { "output": "<a><b>x</b></a>" } } });
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<out>x</out>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn output_string_default() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "output": "string" });
    let input = json!("<a><b>x</b></a>");
    assert!(matches!(
        p.transform_result(&ctx(), &input, &cfg),
        TransformResult::Modified { .. }
    ));
}

#[test]
fn phase_gating() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "phase": "result" });
    let input = json!("<a><b>x</b></a>");
    // phase=result: transform_arguments is a no-op, transform_result fires.
    assert!(matches!(
        p.transform_arguments(&ctx(), &input, &cfg),
        TransformResult::Unchanged
    ));
    assert!(matches!(
        p.transform_result(&ctx(), &input, &cfg),
        TransformResult::Modified { .. }
    ));
}

#[test]
fn empty_stylesheet_is_error() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": "   " });
    assert!(matches!(
        p.transform_result(&ctx(), &json!("<a/>"), &cfg),
        TransformResult::Error { .. }
    ));
}

#[test]
fn missing_stylesheet_is_error() {
    let p = XsltTransform::new("{}");
    assert!(matches!(
        p.transform_result(&ctx(), &json!("<a/>"), &json!({})),
        TransformResult::Error { .. }
    ));
}

#[test]
fn non_string_value_without_input_is_error() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT });
    // The incoming value is an object, not an XML string, and no `input` path.
    assert!(matches!(
        p.transform_result(&ctx(), &json!({ "a": 1 }), &cfg),
        TransformResult::Error { .. }
    ));
}

#[test]
fn missing_input_path_is_error() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "input": "nope" });
    assert!(matches!(
        p.transform_result(&ctx(), &json!({ "xml": "<a/>" }), &cfg),
        TransformResult::Error { .. }
    ));
}

#[test]
fn malformed_xml_is_error() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT });
    assert!(matches!(
        p.transform_result(&ctx(), &json!("<a><b>x</a>"), &cfg),
        TransformResult::Error { .. }
    ));
}

#[test]
fn malformed_xslt_is_error() {
    let p = XsltTransform::new("{}");
    // Well-formed XML, but not an xsl:stylesheet root.
    let cfg = json!({ "stylesheet": "<not-a-stylesheet/>" });
    assert!(matches!(
        p.transform_result(&ctx(), &json!("<a/>"), &cfg),
        TransformResult::Error { .. }
    ));
}

/// A stylesheet that maps `<a>…</a>` to `<out><a>1</a><a>2</a></out>` so the
/// xml_to_json projection sees repeated child tags (→ array).
const TWO_A_TO_OUT: &str = r#"<xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
  <xsl:template match='/r'><out><a>1</a><a>2</a></out></xsl:template>
</xsl:stylesheet>"#;

#[test]
fn output_xml_to_json_arrays_repeated_tags() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": TWO_A_TO_OUT, "output": "xml_to_json" });
    let input = json!("<r/>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => {
            assert_eq!(value, json!({ "out": { "a": ["1", "2"] } }));
        }
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn output_string_still_returns_raw_xml() {
    // With the default/explicit `string` output, the raw serialized XML string
    // is returned unchanged — xml_to_json must not leak into it.
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": TWO_A_TO_OUT, "output": "string" });
    let input = json!("<r/>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => {
            assert_eq!(value, json!("<out><a>1</a><a>2</a></out>"));
        }
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn output_method_text_emits_string_value() {
    // method=text concatenates text nodes, dropping the element markup that the
    // default xml serialization would keep.
    let p = XsltTransform::new("{}");
    let cfg = json!({
        "stylesheet": r#"<xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
  <xsl:template match='/a'><out><xsl:value-of select='b'/></out></xsl:template>
</xsl:stylesheet>"#,
        "output_method": "text"
    });
    let input = json!("<a><b>hello</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("hello")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn output_method_xml_keeps_markup() {
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "output_method": "xml" });
    let input = json!("<a><b>x</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<out>x</out>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn output_method_html_falls_back_to_xml() {
    // xrust has no HTML serializer, so html serializes as XML markup.
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "output_method": "html" });
    let input = json!("<a><b>x</b></a>");
    match p.transform_result(&ctx(), &input, &cfg) {
        TransformResult::Modified { value } => assert_eq!(value, json!("<out>x</out>")),
        other => panic!("expected Modified, got {other:?}"),
    }
}

#[test]
fn xml_to_json_rejects_malformed_output() {
    // The projection itself must fail closed on unbalanced markup rather than
    // panic — defensive even though the XSLT engine emits well-formed XML.
    use super::xml_to_json::xml_to_json;
    assert!(xml_to_json("<out><a>1</out>").is_err());
}

#[test]
fn xml_to_json_text_only_element_is_string() {
    use super::xml_to_json::xml_to_json;
    assert_eq!(
        xml_to_json("<out>hi</out>").unwrap(),
        json!({ "out": "hi" })
    );
}

#[test]
fn xml_to_json_attrs_and_text() {
    use super::xml_to_json::xml_to_json;
    assert_eq!(
        xml_to_json(r#"<out id="7">hi</out>"#).unwrap(),
        json!({ "out": { "@id": "7", "#text": "hi" } })
    );
}

#[test]
fn unknown_config_key_is_rejected() {
    // deny_unknown_fields: a typo'd / stray config key must fail the parse
    // (fail-closed) rather than being silently ignored.
    let p = XsltTransform::new("{}");
    let cfg = json!({ "stylesheet": A_TO_OUT, "phasee": "result" });
    assert!(matches!(
        p.transform_result(&ctx(), &json!("<a/>"), &cfg),
        TransformResult::Error { .. }
    ));
}
