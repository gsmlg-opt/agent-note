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

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SystemConfigSchema {
    #[schema(
        required = false,
        default = json!({"enabled": false, "rules": []})
    )]
    pub duplicate_check: DuplicateCheckConfigSchema,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckConfigSchema {
    #[schema(required = false, default = false)]
    pub enabled: bool,
    #[schema(required = false, default = json!([]), max_items = 64)]
    pub rules: Vec<DuplicateCheckRuleSchema>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckRuleSchema {
    #[schema(min_items = 1, max_items = 32)]
    pub terms: Vec<DuplicateCheckTermSchema>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckTermSchema {
    pub key: String,
    #[schema(required = false)]
    pub value: Option<String>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SystemInfoSchema {
    pub database_engine: String,
    #[schema(required = true)]
    pub database_path: Option<String>,
    #[schema(required = true)]
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    #[schema(required = true)]
    pub attachments_location: Option<String>,
    pub embedding_engine: String,
    pub embedding_model: String,
    pub embedding_fingerprint: String,
}

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
    use std::collections::BTreeSet;
    use tower::ServiceExt;
    use utoipa::openapi::OpenApi;

    fn property_keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .expect("serialized object")
            .keys()
            .cloned()
            .collect()
    }

    fn schema_property_keys(document: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
        document["components"]["schemas"][schema_name]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("missing properties for {schema_name}"))
            .keys()
            .cloned()
            .collect()
    }

    fn schema_required_keys(document: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
        document["components"]["schemas"][schema_name]["required"]
            .as_array()
            .map(|required| {
                required
                    .iter()
                    .map(|key| key.as_str().expect("required property name").to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn assert_nullable_property(
        document: &serde_json::Value,
        schema_name: &str,
        property_name: &str,
    ) {
        let schema_type =
            &document["components"]["schemas"][schema_name]["properties"][property_name]["type"];
        assert!(
            schema_type
                .as_array()
                .expect("nullable OpenAPI 3.1 type array")
                .contains(&serde_json::json!("null")),
            "{schema_name}.{property_name} must accept null"
        );
    }

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

    #[test]
    fn system_schema_fields_match_runtime_serialization() {
        let runtime_config = note_core::SystemConfig {
            duplicate_check: note_core::DuplicateCheckConfig {
                enabled: true,
                rules: vec![note_core::DuplicateCheckRule {
                    terms: vec![note_core::DuplicateCheckTerm {
                        key: "kind".to_string(),
                        value: Some("skill".to_string()),
                    }],
                }],
            },
        };
        let runtime_config = serde_json::to_value(runtime_config).unwrap();
        let runtime_duplicate_check = &runtime_config["duplicate_check"];
        let runtime_rule = &runtime_duplicate_check["rules"][0];
        let runtime_term = &runtime_rule["terms"][0];

        let runtime_info = note_pipelines::SystemInfo {
            database_engine: "embed".to_string(),
            database_path: Some("/tmp/agent-note.db".to_string()),
            database_size_bytes: Some(1024),
            attachments_engine: "filesystem".to_string(),
            attachments_location: Some("/tmp/attachments".to_string()),
            embedding_engine: "stub".to_string(),
            embedding_model: "stub".to_string(),
            embedding_fingerprint: "stub:0".to_string(),
        };
        let runtime_info = serde_json::to_value(runtime_info).unwrap();

        let (_, document) = rest_router();
        let document = serde_json::to_value(document).unwrap();

        assert_eq!(
            property_keys(&runtime_config),
            schema_property_keys(&document, "SystemConfigSchema")
        );
        assert_eq!(
            property_keys(runtime_duplicate_check),
            schema_property_keys(&document, "DuplicateCheckConfigSchema")
        );
        assert_eq!(
            property_keys(runtime_rule),
            schema_property_keys(&document, "DuplicateCheckRuleSchema")
        );
        assert_eq!(
            property_keys(runtime_term),
            schema_property_keys(&document, "DuplicateCheckTermSchema")
        );
        assert_eq!(
            property_keys(&runtime_info),
            schema_property_keys(&document, "SystemInfoSchema")
        );

        assert_eq!(
            schema_required_keys(&document, "SystemConfigSchema"),
            BTreeSet::new()
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckConfigSchema"),
            BTreeSet::new()
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckRuleSchema"),
            BTreeSet::from(["terms".to_string()])
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckTermSchema"),
            BTreeSet::from(["key".to_string()])
        );
        assert_eq!(
            schema_required_keys(&document, "SystemInfoSchema"),
            property_keys(&runtime_info)
        );
        for property in [
            "database_path",
            "database_size_bytes",
            "attachments_location",
        ] {
            assert_nullable_property(&document, "SystemInfoSchema", property);
        }

        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["SystemConfigSchema"]["properties"]["duplicate_check"]["default"],
            serde_json::json!({"enabled": false, "rules": []})
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["enabled"]["default"],
            false
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["rules"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["rules"]["maxItems"],
            note_core::MAX_DUPLICATE_CHECK_RULES
        );
        assert_eq!(
            schemas["DuplicateCheckRuleSchema"]["properties"]["terms"]["minItems"],
            1
        );
        assert_eq!(
            schemas["DuplicateCheckRuleSchema"]["properties"]["terms"]["maxItems"],
            note_core::MAX_DUPLICATE_CHECK_TERMS
        );
    }
}
