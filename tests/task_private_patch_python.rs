use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn task_private_patch_python_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("python3")
        .arg("scripts/test-task-private-patch.py")
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .expect("python3 must execute the task-private patch contract test");
    assert!(status.success(), "task-private patch contract test failed");
}
