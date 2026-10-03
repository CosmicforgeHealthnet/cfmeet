/// Wraps content in the base HTML email layout
pub fn wrap_html(title: &str, content: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title}</title>
</head>
<body style="margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; background-color: #f4f4f5;">
    <table role="presentation" style="width: 100%; border-collapse: collapse;">
        <tr>
            <td align="center" style="padding: 40px 0;">
                <table role="presentation" style="width: 100%; max-width: 600px; border-collapse: collapse; background-color: #ffffff; border-radius: 8px; box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);">
                    <!-- Header -->
                    <tr>
                        <td style="padding: 40px 40px 20px; text-align: center; background-color: #6366f1; border-radius: 8px 8px 0 0;">
                            <h1 style="margin: 0; color: #ffffff; font-size: 28px; font-weight: 700;">CosmicForge</h1>
                        </td>
                    </tr>
                    <!-- Content -->
                    <tr>
                        <td style="padding: 40px;">
                            {content}
                        </td>
                    </tr>
                    <!-- Footer -->
                    <tr>
                        <td style="padding: 20px 40px; text-align: center; background-color: #f4f4f5; border-radius: 0 0 8px 8px;">
                            <p style="margin: 0; color: #71717a; font-size: 12px;">
                                &copy; 2026 CosmicForge HealthNet. All rights reserved.
                            </p>
                        </td>
                    </tr>
                </table>
            </td>
        </tr>
    </table>
</body>
</html>"#,
        title = title,
        content = content
    )
}

/// Creates a CTA button for HTML emails
pub fn button(text: &str, url: &str) -> String {
    format!(
        r#"<table role="presentation" style="width: 100%; border-collapse: collapse;">
    <tr>
        <td align="center">
            <a href="{url}" style="display: inline-block; padding: 16px 32px; background-color: #6366f1; color: #ffffff; text-decoration: none; font-size: 16px; font-weight: 600; border-radius: 6px;">{text}</a>
        </td>
    </tr>
</table>"#,
        text = text,
        url = url
    )
}

/// Wraps content in the base plain text email layout
pub fn wrap_text(content: &str) -> String {
    format!(
        r#"{content}

---
CosmicForge"#,
        content = content
    )
}
