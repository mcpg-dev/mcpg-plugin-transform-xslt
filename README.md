# XSLT Transform (`dev.mcpg.transform.xslt`)

A `transform` plugin that applies an operator-supplied **XSLT stylesheet** to
**input XML** and returns the serialized result. Pure compute — no I/O, no host
calls — so it runs both as a global pre/post-dispatch transform and as a
pipeline `plugin_transform` step. The engine is the pure-Rust
[`xrust`](https://crates.io/crates/xrust) XPath/XSLT implementation (functionally
equivalent to XSLT 1.0 on the XPath 3.1 data model), so it cross-compiles to
every platform and pulls in no system library, FFI, or OpenSSL.

External stylesheet `include`/`import` and `document()` fetches are denied (the
plugin has no I/O); an unresolved external reference surfaces as a transform
error.

## Configuration

| Field | Type | Default | Applies | Description |
|---|---|---|---|---|
| `stylesheet` | string | *(required)* | both | The XSLT stylesheet document text. Supply inline or via `${cred://…}` / `${file://}` (the gateway resolves these before the plugin sees them). |
| `input` | string (JSON Pointer or dotted path) | *(whole value)* | both | Selects the XML **string** within the incoming value, e.g. `"xml"` or `"steps.call.output.response.xml"`. If absent, the incoming value itself must be a JSON string containing the XML. |
| `output` | `"string"` \| `"xml_to_json"` | `"string"` | both | `string`: return the serialized XSLT output as a JSON string. `xml_to_json`: parse the serialized output XML into a JSON value (attributes as `@name`, mixed text as `#text`, repeated child tags as arrays) and return that value. |
| `output_method` | `"xml"` \| `"text"` \| `"html"` | `"xml"` | both | How the result tree is serialized. `xml`: XML markup. `text`: the string value (text nodes concatenated, no markup). `html`: see the note below. The stylesheet's own `<xsl:output method=…>` is **not** honored (xrust 2.1 does not expose it), so set this explicitly. |
| `phase` | `"arguments"` \| `"result"` \| `"both"` | `"both"` | global mode | Which dispatch phase to fire on (ignored in pipeline mode). |
| `max_output_bytes` | int | `1048576` | both | Reject transforms whose serialized output exceeds this. |

Unknown fields are rejected (`deny_unknown_fields`). An empty `stylesheet`,
malformed source XML, or a stylesheet that is not a valid `xsl:stylesheet`
returns an operator-visible error.

## Examples

Transform an XML string field on a tool result, replacing the value with the
serialized output:

```yaml
plugins:
  - id: dev.mcpg.transform.xslt
    class: transform
    source: { oci: "oci://ghcr.io/mcpg-dev/plugins/transform-xslt:protocol-1" }
    config:
      phase: result
      input: response.xml
      stylesheet: |
        <xsl:stylesheet xmlns:xsl='http://www.w3.org/1999/XSL/Transform'>
          <xsl:template match='/a'><out><xsl:value-of select='b'/></out></xsl:template>
        </xsl:stylesheet>
```

`{"response": {"xml": "<a><b>x</b></a>"}}` →
`{"response": "<out>x</out>"}` is *not* produced — the plugin replaces the
**whole** value with the serialized output string. Use a JSONata step after it
if you need to re-nest the result.

As a pipeline step (the `plugin_transform` bridge) reshaping the previous step's
XML output into the next step's input:

```yaml
- kind: plugin_transform
  plugin: dev.mcpg.transform.xslt
  config:
    stylesheet: ${file://./stylesheets/normalize.xsl}
    input: steps.call.output.response.xml
```

## Notes

- The result is serialized per `output_method`: `xml` uses
  `Sequence::to_xml()` (markup), `text` uses `Sequence::to_string()` (the string
  value, text nodes concatenated). With the default `xml`, text-only stylesheets
  still yield their text and literal/`xsl:copy-of` stylesheets yield XML markup.
- **`output_method: html` falls back to XML serialization.** xrust 2.1 ships no
  HTML serializer, so the HTML-specific output rules (void elements written
  without a closing tag, unescaped `<script>`/`<style>` content, etc.) are **not**
  applied — the result is well-formed XML. This is documented rather than
  silently mislabeled; use `text` or `xml` when those rules matter.
- The stylesheet's own `<xsl:output method=…>` declaration is not read — xrust
  2.1 only exposes `indent`, not `method` — so the serialization is driven solely
  by the `output_method` config field.
- Stateless apart from the manifest — the stylesheet and options arrive per call,
  so one instance serves the global chain and the pipeline bridge.
- Pure-Rust (`xrust`), rustls-clean, `default-members` — a vanilla `cargo build`
  includes it and it cross-compiles to every platform.
