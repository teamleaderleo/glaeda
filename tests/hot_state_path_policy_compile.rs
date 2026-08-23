pub use smolrunner::artifact;

include!("../src/hot_state_path_policy.rs");

#[test]
fn hot_state_path_policy_compiles_in_integration_crate() {
    assert_eq!(HOT_STATE_PATH_POLICY_SCHEMA_VERSION, 1);
}
