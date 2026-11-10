use gpui::{AsyncApp, actions};

/// prefix for the lightcode:// url scheme
const GRAM_URL_SCHEME: &str = "lightcode";

actions!(
    cli,
    [
        /// Registers the lightcode:// URL scheme handler.
        RegisterUriScheme
    ]
);

pub async fn register_uri_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.register_url_scheme(GRAM_URL_SCHEME))?.await
}
