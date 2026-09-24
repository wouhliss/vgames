//! Reusable error responses for `#[utoipa::path(responses(...))]`, mirroring
//! `components/responses` in `openapi/openapi.yaml`. Every one is
//! `application/problem+json` with the `Problem` schema.

use std::collections::BTreeMap;

use utoipa::{
    IntoResponses,
    openapi::{ContentBuilder, RefOr, Response, ResponseBuilder, schema::Ref},
};

fn problem(description: &str) -> Response {
    ResponseBuilder::new()
        .description(description)
        .content(
            "application/problem+json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("Problem")))
                .build(),
        )
        .build()
}

macro_rules! problem_response {
    ($name:ident, $status:literal, $description:literal) => {
        #[doc = concat!("`", $status, "` ", $description)]
        pub struct $name;

        impl IntoResponses for $name {
            fn responses() -> BTreeMap<String, RefOr<Response>> {
                BTreeMap::from([($status.to_string(), RefOr::T(problem($description)))])
            }
        }
    };
}

problem_response!(
    BadRequest,
    "400",
    "Invalid request (validation_failed, unknown_field, invalid_cursor, …)"
);
problem_response!(
    Unauthorized,
    "401",
    "Missing, invalid or expired credentials"
);
problem_response!(Forbidden, "403", "Authenticated but not allowed");
problem_response!(
    NotFound,
    "404",
    "Resource does not exist or is not visible to the caller"
);
problem_response!(Conflict, "409", "State conflict");
problem_response!(Gone, "410", "The resource was withdrawn");
problem_response!(
    PreconditionFailed,
    "412",
    "If-Match did not match the current ETag"
);
problem_response!(Unprocessable, "422", "Semantically invalid");
problem_response!(PreconditionRequired, "428", "If-Match header is required");
problem_response!(PayloadTooLarge, "413", "Body or quota limit exceeded");
problem_response!(UnsupportedMediaType, "415", "Unsupported content type");
problem_response!(TooManyRequests, "429", "Rate limited");
