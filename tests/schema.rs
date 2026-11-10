//! The published JSON Schemas of `.cutwater/config.toml` and
//! `.cutwater/versions.toml` match the files the tool reads. After changing
//! either, refresh them with `UPDATE_EXPECT=1 cargo test --test schema`.
//! The paths are absolute: expect-test resolves relative ones against the
//! outermost directory with a `Cargo.toml`, which is wrong when the
//! repository lies inside another Cargo project.

#[test]
fn committed_config_schema_is_current() {
    expect_test::expect_file![concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schema/config.schema.json"
    )]
    .assert_eq(&cutwater::config::json_schema());
}

#[test]
fn committed_manifest_schema_is_current() {
    expect_test::expect_file![concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schema/versions.schema.json"
    )]
    .assert_eq(&cutwater::manifest::json_schema());
}
