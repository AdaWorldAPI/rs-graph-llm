//! Cockpit HTML renderer — dense, professional, everything visible.
//!
//! Design principles from the product vision:
//! 1. Results Are Instruments, Not Output — every cell result renders as the
//!    best visualization for its shape.
//! 2. The Graph Is Primary — force-directed interactive graph, not JSON dump.
//! 3. Information Density — no wasted space. Multiple panels visible. Like a
//!    Bloomberg terminal.
//! 4. Code Is Visible But Compact — one line for simple queries, expandable.
//!
//! Layout: CSS grid with instrument panels. Graph fills main area. Properties
//! sidebar updates on click. Tables are dense, sortable, full-width.

use crate::{Block, Document, RenderError};

/// Render a document to standalone cockpit HTML.
pub fn render_html(doc: &Document) -> Result<String, RenderError> {
    let mut cells_html = String::new();

    for block in &doc.blocks {
        match block {
            Block::Markdown(text) => {
                let parser = pulldown_cmark::Parser::new(text);
                let mut html = String::new();
                pulldown_cmark::html::push_html(&mut html, parser);
                cells_html.push_str(&format!(
                    "<div class=\"cell cell-markdown\">{html}</div>\n"
                ));
            }
            Block::Code { language, source } => {
                let lang_chip = language;
                let line_count = source.lines().count();
                let compact_class = if line_count <= 1 { " compact" } else { "" };
                cells_html.push_str(&format!(
                    r#"<div class="cell cell-code{compact_class}">
  <div class="cell-header">
    <span class="lang-chip">{lang_chip} &#9662;</span>
  </div>
  <pre class="code-block"><code class="language-{language}">{}</code></pre>
</div>
"#,
                    html_escape(source)
                ));
            }
            Block::Output { mime_type, data } => {
                if mime_type == "text/html" {
                    cells_html.push_str(&format!(
                        "<div class=\"cell cell-output instrument\">{data}</div>\n"
                    ));
                } else if mime_type == "application/json" {
                    // JSON output → try to render as sortable table.
                    cells_html.push_str(&format!(
                        "<div class=\"cell cell-output instrument\">{}</div>\n",
                        render_json_table(data)
                    ));
                } else if mime_type.starts_with("text/") {
                    cells_html.push_str(&format!(
                        "<div class=\"cell cell-output instrument\"><pre class=\"output-text\">{}</pre></div>\n",
                        html_escape(data)
                    ));
                } else if mime_type.starts_with("image/") {
                    cells_html.push_str(&format!(
                        "<div class=\"cell cell-output instrument\"><img src=\"data:{mime_type};base64,{data}\" class=\"output-image\" /></div>\n"
                    ));
                }
            }
            Block::Heading { level, text } => {
                cells_html.push_str(&format!(
                    "<h{level} class=\"cell-heading\">{}</h{level}>\n",
                    html_escape(text)
                ));
            }
            Block::GraphVisualization { graph_json } => {
                let id = uuid::Uuid::new_v4().to_string().replace('-', "");
                cells_html.push_str(&format!(
                    r##"<div class="cell cell-graph instrument">
  <div class="graph-panel">
    <div class="graph-container" id="graph-{id}"></div>
    <div class="properties-sidebar" id="props-{id}">
      <div class="props-header">Properties</div>
      <div class="props-content" id="props-content-{id}">
        <span class="props-hint">Click a node to inspect</span>
      </div>
    </div>
  </div>
  <script>
    (function() {{
      var data = {graph_json};
      var container = document.getElementById('graph-{id}');
      var options = {{
        nodes: {{
          shape: 'dot',
          size: 16,
          font: {{ size: 12, color: '#2B2D42' }},
          borderWidth: 2,
          shadow: true
        }},
        edges: {{
          width: 1.5,
          color: {{ inherit: 'both', opacity: 0.7 }},
          arrows: {{ to: {{ enabled: true, scaleFactor: 0.7 }} }},
          font: {{ size: 10, align: 'middle', color: '#666' }},
          smooth: {{ type: 'continuous' }}
        }},
        physics: {{
          forceAtlas2Based: {{
            gravitationalConstant: -30,
            centralGravity: 0.005,
            springLength: 120,
            springConstant: 0.04,
            damping: 0.5
          }},
          solver: 'forceAtlas2Based',
          stabilization: {{ iterations: 150, fit: true }}
        }},
        interaction: {{
          hover: true,
          tooltipDelay: 200,
          multiselect: true,
          navigationButtons: false,
          keyboard: true
        }},
        groups: {{}}
      }};

      // Assign colors per group/label.
      var palette = ['#4361ee','#f72585','#4cc9f0','#7209b7','#3a0ca3','#f77f00','#06d6a0','#e63946'];
      var groupSet = {{}};
      (data.nodes || []).forEach(function(n, i) {{
        var g = n.group || n.label || 'default';
        if (!groupSet[g]) groupSet[g] = palette[Object.keys(groupSet).length % palette.length];
        n.color = {{ background: groupSet[g], border: groupSet[g] }};
      }});

      var network = new vis.Network(container, data, options);

      // Properties sidebar: update on node click.
      network.on('click', function(params) {{
        var propsEl = document.getElementById('props-content-{id}');
        if (params.nodes.length > 0) {{
          var nodeId = params.nodes[0];
          var node = data.nodes.find(function(n) {{ return n.id === nodeId || n.id == nodeId; }});
          if (node) {{
            var html = '<div class="prop-label">' + (node.label || nodeId) + '</div>';
            html += '<div class="prop-id">ID: ' + nodeId + '</div>';
            if (node.group) html += '<div class="prop-group">Group: ' + node.group + '</div>';
            var props = node.properties || {{}};
            Object.keys(props).forEach(function(k) {{
              var val = props[k];
              if (typeof val === 'object') val = JSON.stringify(val);
              html += '<div class="prop-row"><span class="prop-key">' + k + '</span><span class="prop-val">' + val + '</span></div>';
            }});

            // Show connected edges.
            var connected = data.edges.filter(function(e) {{ return e.from == nodeId || e.to == nodeId; }});
            if (connected.length > 0) {{
              html += '<div class="prop-section">Connections (' + connected.length + ')</div>';
              connected.forEach(function(e) {{
                var dir = e.from == nodeId ? '→' : '←';
                var other = e.from == nodeId ? e.to : e.from;
                html += '<div class="prop-edge">' + dir + ' ' + (e.label || '') + ' ' + other + '</div>';
              }});
            }}
            propsEl.innerHTML = html;
          }}
        }} else if (params.edges.length > 0) {{
          var edgeId = params.edges[0];
          var edge = data.edges.find(function(e) {{ return e.id === edgeId; }});
          if (edge) {{
            var html = '<div class="prop-label">' + (edge.label || edgeId) + '</div>';
            html += '<div class="prop-row"><span class="prop-key">from</span><span class="prop-val">' + edge.from + '</span></div>';
            html += '<div class="prop-row"><span class="prop-key">to</span><span class="prop-val">' + edge.to + '</span></div>';
            var props = edge.properties || {{}};
            Object.keys(props).forEach(function(k) {{
              var val = props[k];
              if (typeof val === 'object') val = JSON.stringify(val);
              html += '<div class="prop-row"><span class="prop-key">' + k + '</span><span class="prop-val">' + val + '</span></div>';
            }});
            propsEl.innerHTML = html;
          }}
        }} else {{
          propsEl.innerHTML = '<span class="props-hint">Click a node to inspect</span>';
        }}
      }});
    }})();
  </script>
</div>
"##
                ));
            }
            Block::RawHtml(html) => {
                cells_html.push_str(html);
                cells_html.push('\n');
            }
        }
    }

    let title = doc.title.as_deref().unwrap_or("Notebook");
    let author_meta = doc
        .author
        .as_deref()
        .map(|a| format!("<meta name=\"author\" content=\"{}\">", html_escape(a)))
        .unwrap_or_default();

    Ok(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  {author_meta}
  <title>{title}</title>
  <script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
  <style>
    :root {{
      --bg: #0d1117;
      --surface: #161b22;
      --surface2: #21262d;
      --border: #30363d;
      --text: #c9d1d9;
      --text-muted: #8b949e;
      --accent: #58a6ff;
      --accent2: #f78166;
      --green: #3fb950;
      --code-bg: #0d1117;
      --font-mono: 'SF Mono', 'Cascadia Code', 'Fira Code', Consolas, monospace;
      --font-sans: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif;
    }}

    * {{ margin: 0; padding: 0; box-sizing: border-box; }}

    body {{
      font-family: var(--font-sans);
      background: var(--bg);
      color: var(--text);
      line-height: 1.5;
      padding: 0;
    }}

    /* ── Cockpit grid ─────────────────────────────────────── */
    .cockpit {{
      display: flex;
      flex-direction: column;
      min-height: 100vh;
      max-width: 1400px;
      margin: 0 auto;
      padding: 1rem;
      gap: 0.5rem;
    }}

    .cockpit-header {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 0.5rem 1rem;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 6px;
    }}

    .cockpit-header h1 {{
      font-size: 1rem;
      font-weight: 600;
      color: var(--text);
    }}

    .cockpit-header .meta {{
      font-size: 0.75rem;
      color: var(--text-muted);
    }}

    /* ── Cell base ────────────────────────────────────────── */
    .cell {{
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 6px;
      overflow: hidden;
    }}

    .cell + .cell {{
      margin-top: 2px;
    }}

    /* ── Code cells ───────────────────────────────────────── */
    .cell-code {{
      border-left: 3px solid var(--accent);
    }}

    .cell-code.compact .code-block {{
      padding: 0.25rem 0.75rem;
      max-height: 2.2em;
      overflow: hidden;
    }}

    .cell-code.compact:hover .code-block,
    .cell-code.compact:focus-within .code-block {{
      max-height: none;
    }}

    .cell-header {{
      display: flex;
      align-items: center;
      justify-content: flex-end;
      padding: 0.15rem 0.5rem;
      background: var(--surface2);
      border-bottom: 1px solid var(--border);
    }}

    .lang-chip {{
      font-size: 0.65rem;
      font-family: var(--font-mono);
      color: var(--text-muted);
      background: var(--bg);
      padding: 0.1rem 0.5rem;
      border-radius: 3px;
      cursor: pointer;
      user-select: none;
    }}

    .lang-chip:hover {{
      color: var(--accent);
    }}

    .code-block {{
      margin: 0;
      padding: 0.5rem 0.75rem;
      background: var(--code-bg);
      font-family: var(--font-mono);
      font-size: 0.8rem;
      line-height: 1.6;
      overflow-x: auto;
      color: var(--text);
    }}

    .code-block code {{
      font-family: inherit;
    }}

    /* ── Instrument cells (outputs) ──────────────────────── */
    .instrument {{
      border-left: 3px solid var(--green);
    }}

    .cell-output {{
      padding: 0;
    }}

    .output-text {{
      margin: 0;
      padding: 0.5rem 0.75rem;
      font-family: var(--font-mono);
      font-size: 0.8rem;
      line-height: 1.5;
      color: var(--text);
      background: var(--code-bg);
      white-space: pre-wrap;
      word-break: break-all;
    }}

    .output-image {{
      max-width: 100%;
      display: block;
    }}

    /* ── Graph instrument ─────────────────────────────────── */
    .cell-graph {{
      border-left: 3px solid var(--accent2);
    }}

    .graph-panel {{
      display: grid;
      grid-template-columns: 1fr 260px;
      height: 480px;
    }}

    .graph-container {{
      width: 100%;
      height: 100%;
      background: var(--bg);
    }}

    .properties-sidebar {{
      background: var(--surface2);
      border-left: 1px solid var(--border);
      overflow-y: auto;
      font-size: 0.75rem;
    }}

    .props-header {{
      padding: 0.5rem 0.75rem;
      font-weight: 600;
      font-size: 0.7rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
      color: var(--text-muted);
      border-bottom: 1px solid var(--border);
    }}

    .props-content {{
      padding: 0.5rem 0.75rem;
    }}

    .props-hint {{
      color: var(--text-muted);
      font-style: italic;
    }}

    .prop-label {{
      font-weight: 600;
      font-size: 0.85rem;
      color: var(--accent);
      margin-bottom: 0.25rem;
    }}

    .prop-id, .prop-group {{
      color: var(--text-muted);
      font-family: var(--font-mono);
      font-size: 0.7rem;
      margin-bottom: 0.25rem;
    }}

    .prop-row {{
      display: flex;
      justify-content: space-between;
      gap: 0.5rem;
      padding: 0.2rem 0;
      border-bottom: 1px solid var(--border);
    }}

    .prop-key {{
      color: var(--text-muted);
      font-family: var(--font-mono);
      font-size: 0.7rem;
      flex-shrink: 0;
    }}

    .prop-val {{
      color: var(--text);
      font-family: var(--font-mono);
      font-size: 0.7rem;
      text-align: right;
      word-break: break-all;
    }}

    .prop-section {{
      font-weight: 600;
      font-size: 0.7rem;
      color: var(--text-muted);
      margin-top: 0.75rem;
      margin-bottom: 0.25rem;
      text-transform: uppercase;
      letter-spacing: 0.05em;
    }}

    .prop-edge {{
      font-family: var(--font-mono);
      font-size: 0.7rem;
      color: var(--text);
      padding: 0.15rem 0;
    }}

    /* ── Table instrument ─────────────────────────────────── */
    .result-table {{
      width: 100%;
      border-collapse: collapse;
      font-family: var(--font-mono);
      font-size: 0.75rem;
    }}

    .result-table thead {{
      position: sticky;
      top: 0;
      z-index: 1;
    }}

    .result-table th {{
      background: var(--surface2);
      color: var(--text-muted);
      font-weight: 600;
      text-transform: uppercase;
      letter-spacing: 0.03em;
      font-size: 0.65rem;
      padding: 0.4rem 0.6rem;
      border-bottom: 2px solid var(--border);
      text-align: left;
      cursor: pointer;
      user-select: none;
      white-space: nowrap;
    }}

    .result-table th:hover {{
      color: var(--accent);
    }}

    .result-table th::after {{
      content: ' ↕';
      color: var(--border);
      font-size: 0.6rem;
    }}

    .result-table th.sort-asc::after {{ content: ' ↑'; color: var(--accent); }}
    .result-table th.sort-desc::after {{ content: ' ↓'; color: var(--accent); }}

    .result-table td {{
      padding: 0.3rem 0.6rem;
      border-bottom: 1px solid var(--border);
      color: var(--text);
      max-width: 300px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}

    .result-table tr:hover td {{
      background: var(--surface2);
    }}

    .result-table .type-num {{ color: var(--accent2); text-align: right; }}
    .result-table .type-bool {{ color: #d2a8ff; }}
    .result-table .type-null {{ color: var(--text-muted); font-style: italic; }}

    .table-meta {{
      display: flex;
      justify-content: space-between;
      padding: 0.25rem 0.6rem;
      font-size: 0.65rem;
      color: var(--text-muted);
      background: var(--surface2);
      border-top: 1px solid var(--border);
    }}

    /* ── Markdown cells ───────────────────────────────────── */
    .cell-markdown {{
      padding: 0.75rem 1rem;
      border-left: 3px solid var(--text-muted);
    }}

    .cell-markdown h1, .cell-markdown h2, .cell-markdown h3 {{
      color: var(--text);
      margin-bottom: 0.5rem;
    }}

    .cell-markdown p {{
      margin-bottom: 0.5rem;
    }}

    .cell-markdown code {{
      font-family: var(--font-mono);
      background: var(--bg);
      padding: 0.1rem 0.3rem;
      border-radius: 3px;
      font-size: 0.85em;
    }}

    .cell-heading {{
      color: var(--text);
      padding: 0.5rem 0;
    }}

    /* ── Print / PDF ──────────────────────────────────────── */
    @media print {{
      :root {{
        --bg: #fff;
        --surface: #fff;
        --surface2: #f6f8fa;
        --border: #d0d7de;
        --text: #1f2328;
        --text-muted: #656d76;
        --accent: #0969da;
        --accent2: #cf222e;
        --green: #1a7f37;
        --code-bg: #f6f8fa;
      }}

      body {{ padding: 0; }}
      .cockpit {{ max-width: none; padding: 0; }}
      .cockpit-header {{ display: none; }}

      .graph-panel {{
        height: 350px;
        grid-template-columns: 1fr 200px;
      }}

      .cell {{ break-inside: avoid; page-break-inside: avoid; }}

      .result-table th::after {{ content: ''; }}
    }}

    /* ── Responsive ───────────────────────────────────────── */
    @media (max-width: 768px) {{
      .graph-panel {{
        grid-template-columns: 1fr;
        height: auto;
      }}
      .graph-container {{
        height: 300px;
      }}
      .properties-sidebar {{
        border-left: none;
        border-top: 1px solid var(--border);
        max-height: 200px;
      }}
    }}
  </style>
  <script>
    // Sortable tables: click a th to sort.
    document.addEventListener('click', function(e) {{
      var th = e.target.closest('.result-table th');
      if (!th) return;
      var table = th.closest('table');
      var idx = Array.from(th.parentNode.children).indexOf(th);
      var tbody = table.querySelector('tbody');
      var rows = Array.from(tbody.querySelectorAll('tr'));
      var asc = !th.classList.contains('sort-asc');

      // Clear other sort indicators.
      table.querySelectorAll('th').forEach(function(h) {{
        h.classList.remove('sort-asc', 'sort-desc');
      }});
      th.classList.add(asc ? 'sort-asc' : 'sort-desc');

      rows.sort(function(a, b) {{
        var av = a.children[idx].textContent.trim();
        var bv = b.children[idx].textContent.trim();
        var an = parseFloat(av), bn = parseFloat(bv);
        if (!isNaN(an) && !isNaN(bn)) return asc ? an - bn : bn - an;
        return asc ? av.localeCompare(bv) : bv.localeCompare(av);
      }});

      rows.forEach(function(r) {{ tbody.appendChild(r); }});
    }});
  </script>
</head>
<body>
  <div class="cockpit">
    <div class="cockpit-header">
      <h1>{title}</h1>
      <span class="meta">Polyglot Notebook</span>
    </div>
    {cells_html}
  </div>
</body>
</html>"##
    ))
}

/// Render a JSON array of objects as a sortable HTML table.
fn render_json_table(json_str: &str) -> String {
    let parsed: Result<Vec<serde_json::Value>, _> = serde_json::from_str(json_str);

    // If it's not an array of objects, try wrapping or falling back.
    let rows = match parsed {
        Ok(arr) => arr,
        Err(_) => {
            // Try parsing as a single object.
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(json_str) {
                if obj.is_array() {
                    obj.as_array().cloned().unwrap_or_default()
                } else {
                    vec![obj]
                }
            } else {
                // Not valid JSON — render as preformatted text.
                return format!(
                    "<pre class=\"output-text\">{}</pre>",
                    html_escape(json_str)
                );
            }
        }
    };

    if rows.is_empty() {
        return "<div class=\"table-meta\">Empty result set</div>".to_string();
    }

    // Collect all column names from all rows.
    let mut columns: Vec<String> = Vec::new();
    for row in &rows {
        if let Some(obj) = row.as_object() {
            for key in obj.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }

    if columns.is_empty() {
        // Not objects — render as plain values.
        let items: Vec<String> = rows
            .iter()
            .map(|v| html_escape(&format_json_value(v)))
            .collect();
        return format!(
            "<pre class=\"output-text\">{}</pre>",
            items.join("\n")
        );
    }

    let mut html = String::from("<div style=\"overflow-x:auto\">\n<table class=\"result-table\">\n<thead><tr>");
    for col in &columns {
        html.push_str(&format!("<th>{}</th>", html_escape(col)));
    }
    html.push_str("</tr></thead>\n<tbody>\n");

    for row in &rows {
        html.push_str("<tr>");
        for col in &columns {
            let val = row.get(col);
            let (display, class) = match val {
                Some(serde_json::Value::Number(n)) => (n.to_string(), "type-num"),
                Some(serde_json::Value::Bool(b)) => (b.to_string(), "type-bool"),
                Some(serde_json::Value::Null) | None => ("null".to_string(), "type-null"),
                Some(serde_json::Value::String(s)) => (s.clone(), ""),
                Some(other) => (other.to_string(), ""),
            };
            html.push_str(&format!(
                "<td class=\"{class}\">{}</td>",
                html_escape(&display)
            ));
        }
        html.push_str("</tr>\n");
    }

    html.push_str("</tbody>\n</table>\n</div>\n");
    html.push_str(&format!(
        "<div class=\"table-meta\"><span>{} rows × {} columns</span></div>",
        rows.len(),
        columns.len()
    ));

    html
}

fn format_json_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
