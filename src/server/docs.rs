//! Swagger UI and the OpenAPI document.
//!
//! The UI is loaded from jsDelivr with pinned versions and subresource
//! integrity hashes, so no Swagger crate (or bundled assets) is needed.

use axum::http::header;
use axum::response::{IntoResponse, Redirect, Response};

const OPENAPI: &str = include_str!("openapi.json");

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ansel API</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui.css"
        integrity="sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn" crossorigin="anonymous">
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui-bundle.js"
          integrity="sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep" crossorigin="anonymous"></script>
  <script src="/swagger-ui/init.js"></script>
</body>
</html>
"#;

const INIT: &str =
    r##"window.ui = SwaggerUIBundle({ url: "/api-docs/openapi.json", dom_id: "#swagger-ui" });"##;

/// Allows only the pinned CDN assets and same-origin API calls.
const PAGE_CSP: &str = "default-src 'none'; script-src 'self' https://cdn.jsdelivr.net; \
    style-src https://cdn.jsdelivr.net 'unsafe-inline'; img-src 'self' data: blob:; \
    connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

pub async fn openapi() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], OPENAPI).into_response()
}

pub async fn page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, PAGE_CSP),
        ],
        PAGE,
    )
        .into_response()
}

pub async fn init_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        INIT,
    )
        .into_response()
}

pub async fn redirect() -> Redirect {
    Redirect::permanent("/swagger-ui/")
}

#[cfg(test)]
mod tests {
    #[test]
    fn openapi_is_valid_json() {
        let doc: serde_json::Value = serde_json::from_str(super::OPENAPI).unwrap();
        assert_eq!(doc["openapi"], "3.0.3");
    }
}
