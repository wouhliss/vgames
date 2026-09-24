//! OpenAPI document: base metadata and security schemes; paths come from the handlers.

use axum::Router;
use utoipa::openapi::{
    ComponentsBuilder, InfoBuilder, OpenApi, OpenApiBuilder,
    security::{
        ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
    },
};
use utoipa_swagger_ui::SwaggerUi;

use crate::state::AppState;

/// Title, security schemes and the default security requirement (bearer or cookie).
pub fn base_document() -> OpenApi {
    let components = ComponentsBuilder::new()
        .security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("opaque (vga_…)")
                    .build(),
            ),
        )
        .security_scheme(
            "cookieAuth",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("__Host-vgames_session"))),
        )
        .security_scheme(
            "wsTicket",
            SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::new("ticket"))),
        )
        .build();
    OpenApiBuilder::new()
        .info(
            InfoBuilder::new()
                .title("vgames API")
                .version("1.0.0")
                .description(Some(
                    "Generated from the vgames-api handlers. Contract: openapi/openapi.yaml.",
                ))
                .build(),
        )
        .components(Some(components))
        .security(Some(vec![
            SecurityRequirement::new("bearerAuth", Vec::<String>::new()),
            SecurityRequirement::new("cookieAuth", Vec::<String>::new()),
        ]))
        .build()
}

/// `/openapi.json` and Swagger UI at `/docs` (vendored assets, no CDN).
pub fn docs_routes(doc: OpenApi) -> Router<AppState> {
    Router::new().merge(SwaggerUi::new("/docs").url("/openapi.json", doc))
}
