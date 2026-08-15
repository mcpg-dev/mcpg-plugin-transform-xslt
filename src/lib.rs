//! XSLT transform plugin.
//!
//! Applies an operator-supplied XSLT stylesheet to input XML. Stateless apart
//! from the manifest — the stylesheet + options arrive per call in `config`, so
//! one instance serves both the global transform chain (pre/post dispatch) and
//! the pipeline `plugin_transform` bridge. Pure compute via the pure-Rust
//! `xrust` engine; no host calls, no file/network I/O.

use mcpg_plugin_protocol::{PluginContext, PluginManifest, TransformResult, firstparty_manifest};
use mcpg_plugin_sdk::ffi::SyncTransform;
use serde::Deserialize;
use serde_json::Value;

use xrust::item::{Item, Node, SequenceTrait};
use xrust::parser::ParseError;
use xrust::parser::xml::parse as xml_parse;
use xrust::transform::context::StaticContextBuilder;
use xrust::trees::smite::RNode;
use xrust::xdmerror::{Error as XrustError, ErrorKind};
use xrust::xslt::from_document;

mod xml_to_json;

const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Which dispatch phase(s) a global transform fires on. Ignored by the
/// pipeline bridge (the host calls `transform_result` directly there). An XSLT
/// transform is itself phase-agnostic — both phases route through `run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Arguments,
    Result,
    #[default]
    Both,
}

/// Serialization of the XSLT result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Output {
    /// Return the serialized XSLT output as a JSON string.
    #[default]
    String,
    /// Parse the serialized XSLT output XML into a JSON value (attributes as
    /// `@name`, mixed text as `#text`, repeated child tags as arrays) and return
    /// that value instead of the raw string.
    XmlToJson,
}

/// How the result tree is serialized to a string. xrust 2.1 does not surface the
/// stylesheet's `<xsl:output method=…>` declaration (it parses only `indent`), so
/// the method is an explicit operator override rather than read from the
/// stylesheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputMethod {
    /// Serialize the result tree as XML markup. Default.
    #[default]
    Xml,
    /// Emit the string value of the result (text nodes concatenated, no markup).
    Text,
    /// HTML output. xrust 2.1 has no HTML serializer, so this falls back to XML
    /// serialization; the HTML-specific rules (void elements, unescaped
    /// `<script>`, etc.) are not applied.
    Html,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct XsltConfig {
    /// The XSLT stylesheet document text. The operator supplies it inline or
    /// via `${cred://…}` / `${file://}`, which the gateway resolves before the
    /// plugin sees it.
    stylesheet: String,
    /// JSON Pointer (RFC 6901) selecting the XML string within the incoming
    /// value. A leading-slash path is treated as a pointer; a dotted path is
    /// rewritten to one. If absent, the incoming value must itself be a JSON
    /// string containing the XML.
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    output: Output,
    /// Serialization method for the result tree. Defaults to `xml`. The
    /// stylesheet's own `<xsl:output method=…>` is not honored — xrust 2.1 does
    /// not expose it — so set this explicitly to select text/html.
    #[serde(default)]
    output_method: OutputMethod,
    #[serde(default)]
    phase: Phase,
    #[serde(default = "default_max_output_bytes")]
    max_output_bytes: usize,
}

fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

pub struct XsltTransform {
    manifest: PluginManifest,
}

impl XsltTransform {
    pub fn new(_config_json: &str) -> Self {
        Self {
            manifest: firstparty_manifest! {
                id: "dev.mcpg.transform.xslt",
                name: "XSLT Transform",
                class: Transform,
            },
        }
    }

    fn run(&self, value: &Value, config: &Value, phase: Phase) -> TransformResult {
        let cfg: XsltConfig = match serde_json::from_value(config.clone()) {
            Ok(c) => c,
            Err(e) => {
                return TransformResult::Error {
                    message: format!("xslt transform config: {e}"),
                };
            }
        };
        // Global-mode phase gating; pipeline-mode always calls transform_result.
        if cfg.phase != Phase::Both && cfg.phase != phase {
            return TransformResult::Unchanged;
        }
        if cfg.stylesheet.trim().is_empty() {
            return TransformResult::Error {
                message: "xslt: stylesheet is empty".into(),
            };
        }
        let xml = match select_input_xml(value, cfg.input.as_deref()) {
            Ok(x) => x,
            Err(message) => return TransformResult::Error { message },
        };
        match apply_xslt(
            &cfg.stylesheet,
            &xml,
            cfg.output_method,
            cfg.max_output_bytes,
        ) {
            Ok(out) => {
                let value = match cfg.output {
                    Output::String => Value::String(out),
                    Output::XmlToJson => match xml_to_json::xml_to_json(&out) {
                        Ok(v) => v,
                        Err(message) => {
                            return TransformResult::Error {
                                message: format!("xslt: output XML→JSON failed: {message}"),
                            };
                        }
                    },
                };
                TransformResult::Modified { value }
            }
            Err(message) => TransformResult::Error { message },
        }
    }
}

impl SyncTransform for XsltTransform {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn transform_arguments(
        &self,
        _ctx: &PluginContext,
        arguments: &Value,
        config: &Value,
    ) -> TransformResult {
        self.run(arguments, config, Phase::Arguments)
    }

    fn transform_result(
        &self,
        _ctx: &PluginContext,
        result: &Value,
        config: &Value,
    ) -> TransformResult {
        self.run(result, config, Phase::Result)
    }
}

/// Resolve the input XML string. With `input`, the value at that location must
/// be a JSON string; without it, the whole value must be a JSON string.
fn select_input_xml(value: &Value, input: Option<&str>) -> Result<String, String> {
    let selected = match input {
        Some(path) => {
            let pointer = to_json_pointer(path);
            value
                .pointer(&pointer)
                .ok_or_else(|| format!("xslt: input path {path:?} not found in value"))?
        }
        None => value,
    };
    match selected {
        Value::String(s) => Ok(s.clone()),
        _ => Err(match input {
            Some(path) => format!("xslt: value at {path:?} is not a JSON string"),
            None => {
                "xslt: input value is not a JSON string (set `input` to select an XML field)".into()
            }
        }),
    }
}

/// Accept either a JSON Pointer (`/a/b`) or a dotted path (`a.b`) and normalize
/// to a JSON Pointer.
fn to_json_pointer(path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    let mut out = String::new();
    for seg in path.split('.') {
        out.push('/');
        out.push_str(&seg.replace('~', "~0").replace('/', "~1"));
    }
    out
}

/// Parse `xml`, parse `stylesheet`, run the XSLT transform, serialize the
/// result tree to a string per `method`. Bounded by `max_output_bytes` so an
/// expanding stylesheet can't exhaust memory. Errors are operator-visible and
/// carry no secret material.
fn apply_xslt(
    stylesheet: &str,
    xml: &str,
    method: OutputMethod,
    max_output_bytes: usize,
) -> Result<String, String> {
    let srcdoc =
        parse_xml(xml).map_err(|e| format!("xslt: source XML parse error: {}", e.message))?;
    let styledoc = parse_xml(stylesheet)
        .map_err(|e| format!("xslt: stylesheet parse error: {}", e.message))?;

    // External stylesheet inclusion/import and document() fetches are denied —
    // the plugin has no I/O. `from_document` re-parses included text via the
    // supplied parser closure; the fetch closure returns empty so an
    // unresolved reference surfaces as a transform error rather than I/O.
    let mut ctxt = from_document(styledoc, None, parse_xml, |_| Ok(String::new()))
        .map_err(|e| format!("xslt: stylesheet is not valid XSLT: {}", e.message))?;

    let mut stctxt = StaticContextBuilder::new()
        .message(|_| Ok(()))
        .fetcher(|_| {
            Err(XrustError::new(
                ErrorKind::NotImplemented,
                "external fetch not permitted",
            ))
        })
        .parser(|_| {
            Err(XrustError::new(
                ErrorKind::NotImplemented,
                "external parse not permitted",
            ))
        })
        .build();

    ctxt.context(vec![Item::Node(srcdoc.clone())], 0);
    let result_doc = RNode::new_document();
    ctxt.result_document(result_doc);
    ctxt.populate_key_values(&mut stctxt, srcdoc.clone())
        .map_err(|e| format!("xslt: key setup failed: {}", e.message))?;

    let seq = ctxt
        .evaluate(&mut stctxt)
        .map_err(|e| format!("xslt: transform failed: {}", e.message))?;
    // `text` emits the string value (text nodes concatenated, no markup); `xml`
    // and `html` both XML-serialize, as xrust has no HTML serializer.
    let out = match method {
        OutputMethod::Text => seq.to_string(),
        OutputMethod::Xml | OutputMethod::Html => seq.to_xml(),
    };
    if out.len() > max_output_bytes {
        return Err(format!(
            "xslt output {} bytes exceeds max_output_bytes ({max_output_bytes})",
            out.len()
        ));
    }
    Ok(out)
}

/// Parse an XML document into the smite tree. Namespace resolution against an
/// external catalog is disabled (no I/O); declared namespaces still parse.
fn parse_xml(input: &str) -> Result<RNode, XrustError> {
    let doc = RNode::new_document();
    xml_parse(
        doc.clone(),
        input,
        Some(|_: &_| Err(ParseError::MissingNameSpace)),
    )?;
    Ok(doc)
}

// cdylib export — gated so a plain workspace build emits only the rlib (no
// duplicate `mcpg_plugin_register` symbol across plugin crates).
#[cfg(any(feature = "cdylib-export", feature = "static-firstparty"))]
mcpg_plugin_sdk::declare_plugin! {
    plugin_id: "dev.mcpg.transform.xslt",
    plugin_version: env!("CARGO_PKG_VERSION"),
    descriptor_yaml: include_str!("../plugin.yaml"),
    capabilities: &[],
    entities: [
        transform as xform {
            inner_name: "",
            plugin_type: XsltTransform,
            factory: |cfg: &str, _host: ::mcpg_plugin_sdk::HostHandle| XsltTransform::new(cfg),
        },
    ],
}

#[cfg(test)]
mod tests;
