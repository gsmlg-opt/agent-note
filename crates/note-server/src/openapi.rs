use axum::Router;
use note_pipelines::Context;
use std::sync::Arc;
use utoipa::{
    openapi::{
        schema::{Array, ArrayBuilder, Object, ObjectBuilder, Schema, Type},
        Info, KnownFormat, OpenApi, OpenApiBuilder, RefOr, SchemaFormat, Tag,
    },
    PartialSchema, ToSchema,
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

pub(crate) struct Binary;

impl PartialSchema for Binary {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl ToSchema for Binary {}

pub(crate) fn label_pairs_schema() -> Array {
    Array::new(
        ArrayBuilder::new()
            .items(Object::with_type(Type::String))
            .min_items(Some(2))
            .max_items(Some(2))
            .build(),
    )
}

pub(crate) fn label_pairs_with_empty_default_schema() -> Array {
    ArrayBuilder::from(label_pairs_schema())
        .default(Some(serde_json::json!([])))
        .build()
}

pub(crate) fn label_value_type_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some([
            "text", "number", "version", "date", "datetime", "time",
        ]))
        .description(Some("Label value type"))
        .build()
}

pub(crate) fn note_content_type_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["html"]))
        .description(Some("Optional output format; omission returns Markdown."))
        .build()
}

pub fn rest_router() -> (Router<Arc<Context>>, OpenApi) {
    let openapi = OpenApiBuilder::new()
        .info(Info::new("Agent Note HTTP API", env!("CARGO_PKG_VERSION")))
        .tags(Some([
            tag(
                "notes",
                "Create, list, retrieve, update, search, and download notes",
            ),
            tag("trash", "Restore or permanently delete trashed notes"),
            tag("dashboard", "Read dashboard summary data"),
            tag("rendering", "Render Markdown as HTML"),
            tag("labels", "Manage the label-key catalog"),
            tag("system", "Read and update system configuration and backups"),
        ]))
        .build();

    OpenApiRouter::with_openapi(openapi)
        .merge(crate::notes_api::notes_router())
        .merge(crate::labels_api::labels_router())
        .merge(crate::system_api::system_router())
        .split_for_parts()
}

pub fn swagger_router(openapi: OpenApi) -> Router {
    SwaggerUi::new("/api/docs")
        .url("/api/openapi.json", openapi)
        .into()
}

fn tag(name: &str, description: &str) -> Tag {
    let mut tag = Tag::new(name);
    tag.description = Some(description.to_owned());
    tag
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa::openapi::OpenApi;

    #[tokio::test]
    async fn serves_openapi_document() {
        let response = swagger_router(OpenApi::default())
            .oneshot(
                Request::builder()
                    .uri("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
    }

    #[tokio::test]
    async fn serves_swagger_ui() {
        let app = swagger_router(OpenApi::default());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/docs/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Swagger UI"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/docs/swagger-initializer.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let javascript = String::from_utf8(body.to_vec()).unwrap();
        assert!(javascript.contains("/api/openapi.json"));
    }
}
