//! Shared Cap-inspired HTML chrome for browser-facing pages.

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Cap-style open-ring mark (inline SVG).
pub const LOGO_SVG: &str = r##"<svg class="logo-mark" viewBox="0 0 32 32" width="36" height="36" aria-hidden="true" fill="none">
  <circle cx="16" cy="16" r="12.5" stroke="currentColor" stroke-width="4.5" stroke-linecap="round" stroke-dasharray="58 22" transform="rotate(-48 16 16)"/>
</svg>"##;

/// Base document head bits + design tokens shared by all pages.
pub fn head(title: &str, extra_css: &str) -> String {
    format!(
        r##"<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{title}</title>
<style>
  :root {{
    color-scheme: light;
    --bg: #f5f5f5;
    --bg-elevated: #ffffff;
    --bg-muted: #f0f0f0;
    --ink: #1a1a1a;
    --ink-soft: #5c5c5c;
    --ink-faint: #8a8a8a;
    --line: #e6e6e6;
    --line-strong: #d4d4d4;
    --brand: #3b82f6;
    --brand-soft: #eff6ff;
    --danger: #dc2626;
    --danger-bg: #fef2f2;
    --danger-line: #fecaca;
    --radius: 16px;
    --radius-sm: 10px;
    --shadow: 0 1px 2px rgba(0,0,0,0.04), 0 8px 24px rgba(0,0,0,0.06);
    --shadow-lg: 0 2px 4px rgba(0,0,0,0.04), 0 24px 48px rgba(0,0,0,0.08);
    --font: "Inter", ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ margin: 0; min-height: 100%; }}
  body {{
    font-family: var(--font);
    background: var(--bg);
    color: var(--ink);
    -webkit-font-smoothing: antialiased;
    text-rendering: optimizeLegibility;
  }}
  a {{ color: var(--brand); text-decoration: none; }}
  a:hover {{ text-decoration: underline; }}
  .logo-mark {{ color: var(--brand); display: block; }}
  {extra_css}
</style>
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap" rel="stylesheet" />"##,
        title = html_escape(title),
        extra_css = extra_css,
    )
}
