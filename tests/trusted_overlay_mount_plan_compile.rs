#![cfg(target_os = "linux")]

pub use smolrunner::artifact;
pub use smolrunner::descriptor_bound_launcher;
pub use smolrunner::trusted_overlay_task_view;

#[path = "../src/trusted_overlay_mount_plan.rs"]
mod trusted_overlay_mount_plan;

#[test]
fn trusted_overlay_mount_plan_compiles_in_linux_test_crate() {
    assert_eq!(
        trusted_overlay_mount_plan::TRUSTED_OVERLAY_MOUNT_PLAN_SCHEMA_VERSION,
        1
    );
}
