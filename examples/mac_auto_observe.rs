use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use smolrunner::macos_operator_activity::{
    DEFAULT_OPERATOR_ACTIVE_WINDOW_MILLIS, DEFAULT_OPERATOR_ACTIVITY_FRESHNESS_MILLIS,
    observe_macos_operator_activity,
};
use smolrunner::macos_resource_observation::{
    DEFAULT_FRESHNESS_WINDOW_MILLIS, observe_macos_resources,
};
use smolrunner::process::ShellFreeExecutor;

fn main() -> Result<(), Box<dyn Error>> {
    let now_millis = current_epoch_millis()?;
    let executor = ShellFreeExecutor::new(Duration::from_secs(5), 524_288);

    let activity = observe_macos_operator_activity(
        &executor,
        now_millis,
        now_millis,
        DEFAULT_OPERATOR_ACTIVITY_FRESHNESS_MILLIS,
        DEFAULT_OPERATOR_ACTIVE_WINDOW_MILLIS,
    )?;
    let resources = observe_macos_resources(
        &executor,
        now_millis,
        now_millis,
        DEFAULT_FRESHNESS_WINDOW_MILLIS,
    )?;

    let report = json!({
        "schema_version": 1,
        "report_type": "smolrunner-mac-auto-observation",
        "activity": activity.report(),
        "resources": resources.report(),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn current_epoch_millis() -> Result<u64, Box<dyn Error>> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(u64::try_from(elapsed.as_millis())?)
}
