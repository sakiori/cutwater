//! The published JSON Schema of `.cutwater/config.toml` matches the
//! configuration the tool reads. After changing the configuration, refresh it
//! with `UPDATE_EXPECT=1 cargo test --test schema`.

#[test]
fn committed_schema_is_current() {
    expect_test::expect_file!["../schema/config.schema.json"]
        .assert_eq(&cutwater::config::json_schema());
}
